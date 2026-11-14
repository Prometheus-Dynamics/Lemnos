// SPDX-License-Identifier: GPL-2.0
/*
 * The userspace stand-in of run-watchdog.sh (M36c): what PID 1 becomes after the hook's exec, in
 * place of a slot's lemnosd. argv: the mode (`normal` or `stall`) and the time of the hook's last
 * feed, on CLOCK_MONOTONIC.
 *
 *   normal  opens /dev/watchdog (its first open after the handover), reports the gap from the last
 *           feed, feeds once a second for 45 s, then powers off. The guest staying up that long is the check.
 *   stall   never opens /dev/watchdog. Nothing feeds the device after the hook's last feed, so the
 *           watchdog resets the guest within its timeout. The stall is reported before it starts.
 *
 * It stands in for lemnosd only: lemnosd is not in this guest. Static, so the image needs nothing else.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/reboot.h>
#include <time.h>
#include <unistd.h>

#define STAY_SECONDS 45

static double now(void)
{
	struct timespec ts;

	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec + ts.tv_nsec / 1e9;
}

int main(int argc, char **argv)
{
	const char *mode = argc > 1 ? argv[1] : "normal";
	double last = argc > 2 ? strtod(argv[2], NULL) : 0;
	int fd;

	if (strcmp(mode, "stall") == 0) {
		printf("user: stalled after the handover: /dev/watchdog is not opened; the last feed was %.3f s ago\n",
		       now() - last);
		fflush(stdout);
		for (;;)
			pause();
	}
	fd = open("/dev/watchdog", O_WRONLY | O_CLOEXEC);
	if (fd < 0) {
		printf("user: the first open of /dev/watchdog failed: %s\n", strerror(errno));
		fflush(stdout);
		sleep(60);
		_exit(1);
	}
	printf("user: first open of /dev/watchdog %.3f s after the hook's last feed\n", now() - last);
	fflush(stdout);
	for (int s = 0; s < STAY_SECONDS; s++) {
		if (write(fd, "\0", 1) != 1)
			printf("user: a feed failed: %s\n", strerror(errno));
		sleep(1);
	}
	printf("user: fed the watchdog for %d s and the guest stayed up; powering off\n", STAY_SECONDS);
	fflush(stdout);
	sync();
	reboot(RB_POWER_OFF);
	return 1;
}
