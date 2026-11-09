// SPDX-License-Identifier: GPL-2.0
/*
 * Claims: a second claimant is refused, closing releases, a killed holder
 * releases, malformed names are refused, and their number has an end.
 *
 * Usage: lemnos_claim_check /dev/lemnos
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

#include "../lemnos.h"

#define LIMIT 4096

static int failures;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		int ok_ = !!(cond);                                        \
		printf(ok_ ? "ok   " : "FAIL ");                           \
		failures += !ok_;                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

/* The claim's fd, or -errno. */
static int claim(int fd, const char *name)
{
	struct lemnos_claim c = { 0 };

	strncpy(c.resource, name, sizeof(c.resource) - 1);
	return ioctl(fd, LEMNOS_IOC_CLAIM, &c) ? -errno : c.fd;
}

/*
 * A child that takes `name`, says so on the pipe, and waits to be killed.
 * Through `fd`, or through its own open of `node` when `fd` is negative.
 */
static pid_t holder(int fd, const char *node, const char *name, int *got)
{
	int p[2];
	char ok = 0;

	if (pipe(p))
		return -1;
	pid_t child = fork();

	if (!child) {
		if (fd < 0)
			fd = open(node, O_RDWR);
		char yes = claim(fd, name) >= 0;

		(void)!write(p[1], &yes, 1);
		for (;;)
			pause();
	}
	close(p[1]);
	*got = read(p[0], &ok, 1) == 1 && ok;
	close(p[0]);
	return child;
}

static int killed(pid_t child)
{
	int status = 0;

	kill(child, SIGKILL);
	return waitpid(child, &status, 0) == child && WIFSIGNALED(status) &&
	       WTERMSIG(status) == SIGKILL;
}

int main(int argc, char **argv)
{
	const char *node = argc > 1 ? argv[1] : "/dev/lemnos";
	int fd = open(node, O_RDWR | O_CLOEXEC);
	int got = 0;

	setvbuf(stdout, NULL, _IOLBF, 0);
	if (fd < 0) {
		perror(node);
		return 2;
	}

	int a = claim(fd, "bus-1/target-72");

	CHECK(a >= 0, "first claim held (fd %d)", a);
	CHECK(claim(fd, "bus-1/target-72") == -EBUSY, "second claimant refused (EBUSY)");
	int b = claim(fd, "bus-1/target-73");

	CHECK(b >= 0, "a different name is free");
	CHECK(fcntl(a, F_GETFD) & FD_CLOEXEC, "the claim is close-on-exec");
	close(a);
	a = claim(fd, "bus-1/target-72");
	CHECK(a >= 0, "closing released it");
	close(a);
	close(b);

	/* A copy of the file keeps the claim. */
	a = claim(fd, "copied");
	b = dup(a);
	close(a);
	CHECK(claim(fd, "copied") == -EBUSY, "a copy of the file still holds it");
	close(b);
	a = claim(fd, "copied");
	CHECK(a >= 0, "the last copy closing released it");
	close(a);

	struct lemnos_claim c;

	memset(&c, 0, sizeof(c));
	CHECK(ioctl(fd, LEMNOS_IOC_CLAIM, &c) < 0 && errno == EINVAL, "empty name: EINVAL");
	memset(c.resource, 'x', sizeof(c.resource));
	CHECK(ioctl(fd, LEMNOS_IOC_CLAIM, &c) < 0 && errno == EINVAL, "unterminated name: EINVAL");
	memset(&c, 0, sizeof(c));
	strcpy(c.resource, "flagged");
	c.flags = 1;
	CHECK(ioctl(fd, LEMNOS_IOC_CLAIM, &c) < 0 && errno == EINVAL, "unknown flags: EINVAL");

	/* A crash: another process holds it, is killed, and the claim ends. */
	pid_t child = holder(fd, node, "crash", &got);

	CHECK(got, "a second process holds a claim");
	CHECK(claim(fd, "crash") == -EBUSY, "refused while that process lives");
	CHECK(killed(child), "that process killed");
	a = claim(fd, "crash");
	CHECK(a >= 0, "the crash released it");
	close(a);

	/* The holder of the control node crashes: the node and its claims come free. */
	close(fd);
	child = holder(-1, node, "whole-process", &got);
	CHECK(got, "another process has the node and a claim");
	CHECK(open(node, O_RDWR) < 0 && errno == EBUSY, "the node is refused while it lives");
	CHECK(killed(child), "that process killed");
	fd = open(node, O_RDWR | O_CLOEXEC);
	CHECK(fd >= 0, "the crash freed the node");
	a = claim(fd, "whole-process");
	CHECK(a >= 0, "and the claim");
	close(a);

	/* Claims are kernel memory: their number has an end. */
	struct rlimit lim = { .rlim_cur = 8192, .rlim_max = 8192 };
	int fds[LIMIT], n = 0;
	char name[32];

	CHECK(!setrlimit(RLIMIT_NOFILE, &lim), "room for every claim");
	for (; n < LIMIT; n++) {
		snprintf(name, sizeof(name), "many-%d", n);
		fds[n] = claim(fd, name);
		if (fds[n] < 0)
			break;
	}
	CHECK(n == LIMIT, "%d claims held", n);
	CHECK(claim(fd, "one-too-many") == -ENOSPC, "one more: ENOSPC");
	while (n)
		close(fds[--n]);
	a = claim(fd, "one-too-many");
	CHECK(a >= 0, "closing them made room");
	close(a);
	close(fd);

	printf("%s: %d failed\n", node, failures);
	return failures ? 1 : 0;
}
