# Starts the board's slot from the record the slot driver keeps in U-Boot's own environment
# (record = "uboot"), and never ends at a prompt. Build: mkimage -A <arch> -O linux -T script -C none
# -d boot.cmd boot.scr. Install as BOOT/boot.scr: the built script at the root of the boot partition
# (BOOT is that partition's root; packaging/files.txt). A board that boots from raw NAND takes
# boot-ubi.cmd instead. `lemnos-ctl tree --boot-dir` writes this script with two lines before it that
# set lemnos_usb and lemnos_led from the board file; the lines below give them defaults.
#
# The script takes devtype, devnum and distro_bootpart from U-Boot's standard boot flow. It needs
# sysboot, fdt, test, setenv, saveenv, sleep, fastboot and (with a board LED) led, which the config
# fragment from `lemnos-ctl tree --uboot-config` turns on.
#
# THE RECORD is three variables of U-Boot's redundant environment: default, trial and
# trial_attempts, the boots that may still start the trial. U-Boot loads them with the rest of its
# environment (env/mmc.c, the one GPT partition of type 3de21764-95bd-54bd-a5c3-4abe786f38a8, or the
# board's fixed offsets); the driver and slot-init write them raw, as U-Boot does (see
# drivers/slot-store/src/uboot_env.rs). Nothing here reads or writes a FAT file for the record. Before
# a trial boot the script takes one attempt and saves the environment; only then does it start the
# trial: a boot that never reaches the daemon has used its attempt. A save that fails skips the
# trial. At 0 (or none) it starts the default. `saveenv` writes the copy that is not active and flips
# which is active (env/mmc.c:329-376), so a cut leaves the old or the new copy whole.
#
# THE FIRST ENVIRONMENT. The driver refuses to write an environment U-Boot has not saved: a raw write
# of only our variables would replace U-Boot's defaults, since an import drops what it does not hold
# (env/common.c:443-466, himport_r without H_NOCLEAR unless CONFIG_ENV_APPEND). So the first start
# saves once: when lemnos_env is not set (U-Boot is running on its built-in defaults) the script sets
# it and runs saveenv. From then on the saved environment holds U-Boot's variables, ours, and
# lemnos_env.
#
# THE BOOT SOURCE. The slot's device tree is board.dtb (the fixed contract name) and no label has an
# fdt line. Before each label starts, the script names the boot source in the device tree it hands
# on: the device-tree path of the controller this boot came from, which U-Boot's own control tree
# gives as the alias mmc<devnum> (the same number as `mmc ${devnum}`). The kernel finds the boot disk
# from that path through the board file's boot_sources. A non-mmc boot device is refused by name: the
# path form cannot name it.
#
# NEVER AT A PROMPT. A label that cannot start (its board.dtb will not load, the tree will not take
# the boot source, sysboot returns) is reported by name and the script goes on: trial, then default,
# then the root copy. When none starts, or the boot source cannot be named at all, the script offers
# the board for flashing and then tries again, for ever: it is one `while` whose body ends in
# `fastboot usb`, a sleep, and the next pass. The only ways out are a slot starting (sysboot does not
# return) or a reset. The first two passes that start nothing only pause and try again, the third
# offers download mode, and each later pass tries the slots once before it offers it again.
#   `fastboot usb <n>` registers the USB gadget and waits (cmd/fastboot.c:96-117). The command has no
#   time limit: it returns when the host sends `fastboot continue` (drivers/fastboot/fb_command.c:75-78,
#   drivers/usb/gadget/f_fastboot.c:566-567, g_dnl_trigger_detach) or on Ctrl-C; `fastboot reboot`
#   resets the board (drivers/fastboot/fb_common.c:205-209). With no USB cable it returns at once when
#   the board's g_dnl_board_usb_cable_connected says so (cmd/fastboot.c:96-101; the default,
#   drivers/usb/gadget/g_dnl.c:180-183, does not). Every return starts the next pass over the slots.
#   The watchdog: with CONFIG_WATCHDOG the loop calls schedule() (cmd/fastboot.c:115) which runs the
#   watchdog's cyclic servicing (common/cyclic.c:118-131), so a flash in progress is never reset, and
#   a U-Boot that stops scheduling (a hang) is reset after CONFIG_WATCHDOG_TIMEOUT_MSECS.
#   The LED, when the board file names one (lemnos_led), is on while the board waits (cmd/led.c:121-131).
#   hush has `while` and `for` (common/cli_hush.c:2383-2388), no functions, and `break` and `continue`
#   exist but do nothing (common/cli_hush.c:501,503), so the loop is the one `while` and the steps
#   run as a `for` over the three choices.
#
# Commands, from U-Boot v2026.10: `fdt resize <extrasize>` takes its argument in hex and shrinks the
# tree to its size plus a 4 KiB rounding plus that many bytes (cmd/fdt.c:775-783, help at
# cmd/fdt.c:1129); `fdt set` fails when the tree has no room (the bytes added here make room for the
# boot-source property).

if test -z "${lemnos_usb}"; then
	setenv lemnos_usb 0
fi

if test -z "${lemnos_env}"; then
	setenv lemnos_env 1
	if saveenv; then
		echo "lemnos: saved the first U-Boot environment"
	else
		setenv lemnos_env
		echo "lemnos: the first U-Boot environment could not be saved, going on"
	fi
fi

setenv lemnos_pass
setenv lemnos_go 1
while test "${lemnos_go}" = "1"; do
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
		echo "lemnos: ${lemnos_why}, starting no slot"
	else
		setenv slot_part ${devnum}:${distro_bootpart}
		setenv left
		setenv take_trial
		setenv take_default

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
				setenv left
				if saveenv; then
					setenv take_trial ${trial}
				fi
			fi
		fi
		setenv take_default ${default}

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
	fi

	# Nothing started. The first two passes just try again after a pause (a card that answered late, a
	# read that failed once); the third offers download mode, and after the host ends the session
	# (or fastboot returns) the next pass tries the slots once more before offering it again.
	setenv lemnos_pass ${lemnos_pass}x
	if test "${lemnos_pass}" = "xxx"; then
		echo "lemnos: no slot started: offering download mode on USB ${lemnos_usb}"
		if test -n "${lemnos_led}"; then
			led ${lemnos_led} on
		fi
		fastboot usb ${lemnos_usb}
		if test -n "${lemnos_led}"; then
			led ${lemnos_led} off
		fi
		setenv lemnos_pass xx
	else
		echo "lemnos: no slot started (pass ${lemnos_pass}), trying again"
	fi
	sleep 2
done
