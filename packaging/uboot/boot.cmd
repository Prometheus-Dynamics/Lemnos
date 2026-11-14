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
#
# The boot source. The slot's device tree is board.dtb (the fixed contract name) and no label has
# an fdt line. Before each label starts, the script names the boot source in the device tree it
# hands on: the device-tree path of the controller this boot came from, which U-Boot's own control
# tree gives as the alias mmc<devnum> (the same number as `mmc ${devnum}`). The kernel finds the
# boot disk from that path through the board file's boot_sources. A non-mmc boot device is refused
# by name: the path form cannot name it.
#
# A refusal never ends at a prompt. A label that cannot start (its board.dtb will not load, the
# tree will not take the boot source, sysboot returns) is reported by name and the script goes on to
# the next choice: trial, then default, then the root copy. The one case that cannot fall through is
# a boot source that cannot be named at all: nothing can boot safely, so the script ends there, in
# ONE place (below). Running out of choices ends the script too (U-Boot's own boot flow goes on).
#
# Commands, from U-Boot v2026.10: `fdt resize <extrasize>` takes its argument in hex and shrinks the
# tree to its size plus a 4 KiB rounding plus that many bytes (cmd/fdt.c:775-783, help at
# cmd/fdt.c:1129); `fdt set` fails when the tree has no room (the bytes added here make room for
# the boot-source property). `for ... in ...; do ... done` and `elif` are in U-Boot's hush (common/cli_hush.c:2380-2386); it has no
# functions, so the steps run as a loop over the three choices.

setenv lemnos_src
setenv lemnos_why
if test "${devtype}" != "mmc"; then
	setenv lemnos_why "boot device ${devtype} is unsupported: only mmc boot sources are named"
else
	fdt addr -c
	fdt get value lemnos_src /aliases ${devtype}${devnum}
	if test -z "${lemnos_src}"; then
		setenv lemnos_why "this control device tree has no alias ${devtype}${devnum}: the boot source is not named"
	fi
fi
if test -n "${lemnos_why}"; then
	echo "lemnos: ${lemnos_why}, refusing"
	# I5i7: replaced by the download-mode loop
	exit
fi

setenv slot_part ${devnum}:${distro_bootpart}
setenv default
setenv trial
setenv trial_attempts
setenv left
setenv take_trial
setenv take_default

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
				setenv take_trial ${trial}
			fi
		fi
	fi
	setenv take_default ${default}
fi

for step in trial default root; do
	setenv lemnos_dtb
	if test "${step}" = "trial"; then
		if test -n "${take_trial}"; then
			setenv lemnos_dtb /slots/${take_trial}/board.dtb
			setenv lemnos_conf /slots/${take_trial}/extlinux/extlinux.conf
		fi
	elif test "${step}" = "default"; then
		if test -n "${take_default}"; then
			setenv lemnos_dtb /slots/${take_default}/board.dtb
			setenv lemnos_conf /slots/${take_default}/extlinux/extlinux.conf
		fi
	else
		setenv lemnos_dtb /extlinux/board.dtb
		setenv lemnos_conf /extlinux/extlinux.conf
	fi
	if test -n "${lemnos_dtb}"; then
		if load ${devtype} ${slot_part} ${fdt_addr_r} ${lemnos_dtb}; then
			fdt addr ${fdt_addr_r}
			fdt resize 400
			if fdt set /chosen lemnos,boot-source "${lemnos_src}"; then
				setenv fdt_addr ${fdt_addr_r}
				sysboot ${devtype} ${slot_part} any ${pxefile_addr_r} ${lemnos_conf}
				echo "lemnos: ${step}: ${lemnos_conf} did not start, going on"
			else
				echo "lemnos: ${step}: the boot source did not fit in ${lemnos_dtb}, going on"
			fi
		else
			echo "lemnos: ${step}: cannot load ${lemnos_dtb}, going on"
		fi
	fi
done
# Out of choices. I5i7: replaced by the download-mode loop.
