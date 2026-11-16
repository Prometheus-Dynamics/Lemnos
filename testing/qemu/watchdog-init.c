// SPDX-License-Identifier: GPL-2.0
/*
 * The watchdog hook of run-watchdog.sh (M36c): the guest's first process in the `normal` and `stall`
 * modes. It does what slot-init does for the watchdog, and nothing else: it mounts proc, sysfs and
 * devtmpfs, opens /dev/watchdog (an open arms it, and the descriptor is close-on-exec, as Rust's
 * OpenOptions makes it), sets the timeout to 15 s and reads it back, feeds it with byte 0 (never `V`)
 * while it works, and then hands PID 1 to the userspace stand-in by exec, the way switch_root does.
 * Its last feed is made right before the exec, as slot-init's is (no wait between them). The last feed's
 * time goes to the stand-in, which reports the gap from it to its own first feed (run-watchdog.sh checks
 * that gap against the applied period).
 *
 * It does not run slot-init's boot: the slot is not in the guest. Static, so the image needs nothing else.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <time.h>
#include <unistd.h>

/* _IOWR('W', 6, int) and _IOR('W', 7, int), as lemnos-driver-hw-watchdog-sys has them. */
#define WDIOC_SETTIMEOUT 0xC0045706u
#define WDIOC_GETTIMEOUT 0x80045707u
#define DOG_SECONDS 15
#define FEEDS 4

static double now(void)
{
	struct timespec ts;

	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec + ts.tv_nsec / 1e9;
}

static void feed(int fd)
{
	if (write(fd, "\0", 1) != 1)
		printf("hook: a feed failed: %s\n", strerror(errno));
}

/* The stand-in's mode, from the command line: `lemnos.watchdog=stall` or the default `normal`. */
static const char *mode(void)
{
	char line[512] = "";
	FILE *f = fopen("/proc/cmdline", "r");

	if (f) {
		if (!fgets(line, sizeof line, f))
			line[0] = '\0';
		fclose(f);
	}
	return strstr(line, "lemnos.watchdog=stall") ? "stall" : "normal";
}

int main(void)
{
	int fd, timeout = DOG_SECONDS, applied = 0;
	double last = 0;
	char when[32];

	mount("proc", "/proc", "proc", 0, NULL);
	mount("sysfs", "/sys", "sysfs", 0, NULL);
	mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
	fd = open("/dev/watchdog", O_WRONLY | O_CLOEXEC);
	if (fd < 0) {
		printf("hook: NO WATCHDOG: %s\n", strerror(errno));
		fflush(stdout);
		_exit(1);
	}
	if (ioctl(fd, WDIOC_SETTIMEOUT, &timeout) < 0 || ioctl(fd, WDIOC_GETTIMEOUT, &applied) < 0) {
		printf("hook: the timeout can't be set: %s\n", strerror(errno));
		fflush(stdout);
		_exit(1);
	}
	printf("hook: watchdog armed: %d s\n", applied);
	for (int i = 0; i < FEEDS; i++) {
		feed(fd);
		sleep(1);
	}
	/* The last feed, right before the exec: nothing waits between it and the hand-over. */
	feed(fd);
	last = now();
	snprintf(when, sizeof when, "%.6f", last);
	printf("hook: last feed at %s, handing over to /sbin/watchdog-user\n", when);
	fflush(stdout);
	execl("/sbin/watchdog-user", "watchdog-user", mode(), when, (char *)NULL);
	printf("hook: exec failed: %s\n", strerror(errno));
	fflush(stdout);
	_exit(1);
}
