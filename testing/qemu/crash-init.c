// SPDX-License-Identifier: GPL-2.0
/*
 * The crash-log hook of run-crash.sh (BB3): the guest's first process. It mounts proc, sysfs and devtmpfs,
 * mounts pstore at /sys/fs/pstore (a systemd image does that itself; this guest has no systemd, so the hook
 * stands in for it), and mounts the state disk (ext4, the whole disk) at /state, which keeps the boot history
 * from one reset to the next. Then it runs the probe, which is the driver's own crash-log record step.
 *
 * Modes, from the command line:
 *   default      the crash run. The first boot (no /state/armed) arms the marker, runs the probe (its
 *                history entry: the boot before the crash), syncs and writes 'c' to /proc/sysrq-trigger.
 *                The panic reboots the guest (panic=1) and QEMU keeps the RAM, so the next boot finds the
 *                record. That boot runs the probe, which stores the record and clears pstore, and powers off.
 *   lemnos.plain one probe and power off, with no crash.
 *   lemnos.nomount pstore is not mounted (the "not mounted" case; with lemnos.plain).
 *   lemnos.undeclared the probe runs with no pstore (the board declares no crash log).
 *
 * Static, so the initramfs needs nothing else. It is not the watchdog hook (watchdog-init.c).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int cmdline_has(const char *word)
{
	char line[1024] = "";
	FILE *f = fopen("/proc/cmdline", "r");

	if (f) {
		if (!fgets(line, sizeof line, f))
			line[0] = '\0';
		fclose(f);
	}
	return strstr(line, word) != NULL;
}

static void say(const char *text)
{
	fprintf(stdout, "crash-init: %s\n", text);
	fflush(stdout);
}

/* Runs the probe; its output goes to the console. Returns its status. */
static int probe(int declared)
{
	char *argv[] = { "/sbin/crash-probe", "/state/data", "/state/boot", "/sys/fs/pstore", NULL, NULL };
	pid_t pid;
	int status = 0;

	if (!declared) {
		argv[3] = "--no-pstore";
	}
	pid = fork();
	if (pid < 0) {
		say("fork failed");
		return -1;
	}
	if (pid == 0) {
		execv(argv[0], argv);
		_exit(127);
	}
	if (waitpid(pid, &status, 0) < 0)
		return -1;
	fprintf(stdout, "crash-init: probe exited %d\n", WIFEXITED(status) ? WEXITSTATUS(status) : -1);
	fflush(stdout);
	return status;
}

static void power_off(void)
{
	sync();
	reboot(RB_POWER_OFF);
	for (;;)
		pause();
}

int main(void)
{
	int declared, plain;
	int tries;
	struct stat st;

	mount("proc", "/proc", "proc", 0, NULL);
	mount("sysfs", "/sys", "sysfs", 0, NULL);
	mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
	/* The command line is read from /proc, so the flags are read after it is mounted. */
	declared = !cmdline_has("lemnos.undeclared");
	plain = cmdline_has("lemnos.plain");
	if (!cmdline_has("lemnos.nomount")) {
		mkdir("/sys/fs/pstore", 0755);
		if (mount("pstore", "/sys/fs/pstore", "pstore", 0, NULL) != 0)
			fprintf(stdout, "crash-init: mount pstore: %s\n", strerror(errno));
	}
	/* The state disk: the device appears once the driver is up, so wait for it. */
	for (tries = 0; tries < 200; tries++) {
		if (stat("/dev/nvme0n1", &st) == 0)
			break;
		usleep(100000);
	}
	mkdir("/state", 0755);
	if (mount("/dev/nvme0n1", "/state", "ext4", 0, NULL) != 0) {
		fprintf(stdout, "crash-init: mount state: %s\n", strerror(errno));
		power_off();
	}
	mkdir("/state/data", 0755);
	mkdir("/state/boot", 0755);

	if (plain) {
		probe(declared);
		power_off();
	}
	if (stat("/state/armed", &st) != 0) {
		int fd = open("/state/armed", O_CREAT | O_WRONLY, 0644);

		if (fd < 0) {
			say("cannot arm the crash");
			power_off();
		}
		write(fd, "1\n", 2);
		fsync(fd);
		close(fd);
		say("arming the crash: the first boot");
		probe(declared);
		sync();
		say("crashing the guest (sysrq-c)");
		fd = open("/proc/sysrq-trigger", O_WRONLY);
		if (fd >= 0)
			write(fd, "c", 1);
		/* Not reached: the panic reboots the guest. */
		for (;;)
			pause();
	}
	say("the boot after the crash");
	probe(declared);
	say("done");
	power_off();
	return 0;
}
