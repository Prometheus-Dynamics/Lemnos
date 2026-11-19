# U-Boot marker: the patched U-Boot's default environment carries lemnos_env_rev=1 (uboot::ENV_MARKER,
# uboot::ENV_MARKER_VALUE). This script does not read it. The driver (system-slots, trial updates) reads it from
# the stored environment and reports loader-not-updated while it is absent. docs/boards.md, "What an image
# builder checks", names the check; docs/slots.md, "Trial updates on the uboot record", the reasons.
# Starts the board's slot from the record the slot driver keeps in U-Boot's own environment
# (record = "uboot"), and never ends at a prompt. Build: mkimage -A <arch> -O linux -T script -C none
# -d boot.cmd boot.scr. Install as BOOT/boot.scr: the built script at the root of the boot partition
# (BOOT is that partition's root; packaging/files.txt). A board that boots from raw NAND takes
# boot-ubi.cmd instead. `lemnos-ctl boot-partition` writes this script (read from packaging/uboot) into the
# image, and `lemnos-ctl tree --uboot-config` and the board file give lemnos_usb and lemnos_led; the lines
# below give them defaults.
#
# The script takes devtype, devnum and distro_bootpart from U-Boot's standard boot flow. It needs
# sysboot, fdt, test, part (start, for the environment's partition), echo, setenv, saveenv, sleep, fastboot and (with a board LED) led, which the config
# fragment from `lemnos-ctl tree --uboot-config` turns on.
#
# THE RECORD is these variables of U-Boot's redundant environment: default, trial and
# trial_attempts, the boots that may still start the trial, and default_bay, previous_bay and trial_bay,
# the payload bay (`slots/bayN/` on the boot partition, N from 0) that holds each slot's files. A slot's
# files are only ever in a bay, so each step below reads `/slots/<bay>/board.dtb` and
# `/slots/<bay>/extlinux/extlinux.conf`; the bays are written by the slot driver, raw, and only read here.
# U-Boot loads them with the rest of its
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
# THE THREE STEPS, in this order: the trial (when it has an attempt left), the default, and the
# previous slot, which is started AS A TRIAL: it is not a slot the loader promotes. The third step
# runs only when a previous bay is named (`previous_bay`), is not the default's bay and is not the
# trial's bay the first step already tried: the three conditions compare bays, which the writers keep
# (the environment holds no id for the previous slot; a test evaluates these conditions over the writers'
# environment, and a real U-Boot has not run them: untested on board). It saves nothing and changes nothing in the stored
# environment (no saveenv, and `trial`, `trial_attempts`, `default` and the bays are not
# touched, so a trial still pending keeps its attempts). It starts the previous bay's kernel, with the
# device tree and extlinux.conf read from that bay, and, when the default had been named and did not
# start, it adds the property `lemnos,default-failed = "files"` to /chosen of the device tree it hands
# on. slot-init reads that property: it finds a slot that is not the default (which is what marks a
# trial boot), records the default's failure as "the boot loader could not load the default's files",
# and leaves it to the daemon to confirm the previous slot as it confirms any trial.
#
# NEVER AT A PROMPT. A label that cannot start (its board.dtb will not load, the tree will not take
# the boot source, sysboot returns) is reported by name and the script goes on: trial, default, then
# previous, and nothing else (no copy of a kernel is kept at the partition's root). When none starts, or the boot source cannot be named at all, the script offers
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

# A save is made only where the environment has a partition. `saveenv` writes the environment on the device U-Boot
# keeps it on (CONFIG_ENV_MMC_DEVICE_INDEX, the image's `lemnos_envdev`); without a uboot-env partition on that device,
# its redundant copy falls back to offset 0x0, the partition table. `part start` with a name is in U-Boot v2026.10
# (cmd/part.c, do_part_start, line 487) and the Radxa tree (same lines); it is false when there is no such partition.
# Not saved means the boot goes on: a trial is not started (it could not be recorded), and the default boots.
if test -z "${lemnos_envdev}"; then
	setenv lemnos_envdev 0
fi
setenv lemnos_saveable
if part start mmc ${lemnos_envdev} uboot-env lemnos_envpart; then
	setenv lemnos_saveable 1
else
	echo "lemnos: no uboot-env partition on mmc ${lemnos_envdev}: the environment is not saved"
fi

if test -z "${lemnos_env}"; then
	setenv lemnos_env 1
	if test -n "${lemnos_saveable}"; then
		if saveenv; then
			echo "lemnos: saved the first U-Boot environment"
		else
			setenv lemnos_env
			echo "lemnos: the first U-Boot environment could not be saved, going on"
		fi
	else
		setenv lemnos_env
	fi
fi

# THE RESTART COUNT (BB2, the Pi firmware's boot_count in this form). lemnos_restarts is the boots
# attempted since the last good boot, this one included, saturating at 8. The rungs are thresholds on the count, not
# bits of it: at 4 to 7 the previous starts first, at 8 and above the rescue holds (a bit test of 4 is true again at
# 12 to 15, so a count past 8 would swing between rungs). It is raised here, once per
# script run, with one saveenv. When the image names the reset-status word (lemnos_rst, a SoC register:
# see docs/slots.md, "The U-Boot restart count"), a power-on reset sets the count to 0 instead of
# raising it, as the Pi firmware's count is lost at power loss. Without lemnos_rst every boot raises it.
# lemnosd clears it at a good boot and before its own restart; slot-init never writes it.
if test -z "${lemnos_restarts}"; then
	setenv lemnos_restarts 0
fi
if test -n "${lemnos_rst}"; then
	setexpr.l lemnos_rst_now *${lemnos_rst}
	if test "${lemnos_rst_now}" = "0"; then
		if test "${lemnos_restarts}" != "0"; then
			setenv lemnos_restarts 0
			if test -n "${lemnos_saveable}"; then
				saveenv
			fi
		fi
	elif test "${lemnos_restarts}" -lt 8; then
		setexpr lemnos_restarts "${lemnos_restarts}" + 1
		if test -n "${lemnos_saveable}"; then
			saveenv
		fi
	fi
elif test "${lemnos_restarts}" -lt 8; then
	setexpr lemnos_restarts "${lemnos_restarts}" + 1
	if test -n "${lemnos_saveable}"; then
		saveenv
	fi
fi

setenv lemnos_pass
setenv lemnos_go 1
while test "${lemnos_go}" = "1"; do
	setenv lemnos_src
	setenv lemnos_why
	setenv lemnos_dflt
	# At 8 the board is rescued: no slot starts, and download mode is offered at once (the host
	# flasher is the exit). At 4 to 7 (the Pi's [boot_count>3]) the previous starts before the default.
	setenv lemnos_rescue
	if test "${lemnos_restarts}" -ge 8; then
		setenv lemnos_rescue 1
	fi
	if test "${devtype}" != "mmc"; then
		setenv lemnos_why "boot device ${devtype} is unsupported: only mmc boot sources are named"
	else
		fdt addr -c
		fdt get value lemnos_src /aliases ${devtype}${devnum}
		if test -z "${lemnos_src}"; then
			setenv lemnos_why "this control device tree has no alias ${devtype}${devnum}: the boot source is not named"
		fi
	fi

	if test -n "${lemnos_rescue}"; then
		echo "lemnos: rescue after ${lemnos_restarts} boots without a good boot: starting no slot, download mode on USB ${lemnos_usb}"
		setenv lemnos_pass "xx"
	elif test -n "${lemnos_why}"; then
		echo "lemnos: ${lemnos_why}, starting no slot"
	else
		setenv slot_part ${devnum}:${distro_bootpart}
		setenv left
		setenv take_trial
		setenv take_default
		setenv take_trial_bay
		setenv take_default_bay
		setenv take_previous_bay

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
				if test -n "${lemnos_saveable}"; then
					if saveenv; then
						setenv take_trial ${trial}
						setenv take_trial_bay ${trial_bay}
					fi
				fi
			fi
		fi
		setenv take_default ${default}
		setenv take_default_bay ${default_bay}
		if test -n "${previous_bay}"; then
			if test "${previous_bay}" != "${default_bay}"; then
				if test "${previous_bay}" != "${take_trial_bay}"; then
					setenv take_previous_bay ${previous_bay}
				fi
			fi
		fi

		# The rung at 4 to 7: the previous starts first. The default is skipped in the first pass only when
		# a previous is named; when that previous does not start, the next pass starts the default.
		setenv lemnos_window
		if test "${lemnos_restarts}" -ge 4; then
			if test "${lemnos_restarts}" -lt 8; then
				setenv lemnos_window 1
			fi
		fi
		if test -n "${lemnos_window}"; then
			if test -n "${take_previous_bay}"; then
				if test -z "${lemnos_pass}"; then
					setenv take_default
				fi
			fi
		fi

		for step in trial default previous; do
			setenv lemnos_dtb
			if test "${step}" = "trial"; then
				if test -n "${take_trial}"; then
					setenv lemnos_dtb /slots/${take_trial_bay}/board.dtb
					setenv lemnos_conf /slots/${take_trial_bay}/extlinux/extlinux.conf
				fi
			elif test "${step}" = "default"; then
				if test -n "${take_default}"; then
					setenv lemnos_dtb /slots/${take_default_bay}/board.dtb
					setenv lemnos_conf /slots/${take_default_bay}/extlinux/extlinux.conf
					setenv lemnos_dflt files
				fi
			else
				if test -n "${take_previous_bay}"; then
					setenv lemnos_dtb /slots/${take_previous_bay}/board.dtb
					setenv lemnos_conf /slots/${take_previous_bay}/extlinux/extlinux.conf
					if test -n "${lemnos_window}"; then
						setenv lemnos_dflt restarts
					fi
				fi
			fi
			if test -n "${lemnos_dtb}"; then
				if load ${devtype} ${slot_part} ${fdt_addr_r} ${lemnos_dtb}; then
					fdt addr ${fdt_addr_r}
					fdt resize 400
					if fdt set /chosen lemnos,boot-source "${lemnos_src}"; then
						if test "${step}" = "previous"; then
							if test -n "${lemnos_dflt}"; then
								fdt set /chosen lemnos,default-failed "${lemnos_dflt}"
							fi
						fi
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
