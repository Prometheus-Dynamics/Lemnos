// SPDX-License-Identifier: GPL-2.0
/*
 * The guest's first process for a kernel with lemnos built in and no lemnos.ko. The module's
 * parameters come from the kernel command line, not from insmod. Checks that the module is
 * built in, that its control node exists before any daemon starts, that the command-line
 * parameters took, and that the claim checks pass. Prints one result line and powers off.
 * Static, so the image needs nothing else.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/wait.h>
#include <unistd.h>

static int failures;

static void check(int ok, const char *what)
{
	printf("%s %s\n", ok ? "ok  " : "FAIL", what);
	failures += !ok;
}

/* The value of a module parameter, or "" when it cannot be read. */
static void parameter(const char *name, char *buf, size_t size)
{
	char path[128];
	FILE *f;

	snprintf(path, sizeof(path), "/sys/module/lemnos/parameters/%s", name);
	buf[0] = '\0';
	f = fopen(path, "r");
	if (!f)
		return;
	if (fgets(buf, (int)size, f))
		buf[strcspn(buf, "\n")] = '\0';
	fclose(f);
}

static int run(char *const argv[])
{
	pid_t pid = fork();
	int status = 0;

	if (pid == 0) {
		execv(argv[0], argv);
		_exit(127);
	}
	if (pid < 0 || waitpid(pid, &status, 0) < 0)
		return -1;
	return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

int main(void)
{
	char value[256];
	int con;

	mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
	mount("sysfs", "/sys", "sysfs", 0, NULL);
	mount("proc", "/proc", "proc", 0, NULL);
	con = open("/dev/console", O_RDWR);
	if (con >= 0) {
		dup2(con, 0);
		dup2(con, 1);
		dup2(con, 2);
	}
	setvbuf(stdout, NULL, _IONBF, 0);
	printf("lemnos-builtin: start\n");

	check(!access("/sys/module/lemnos", F_OK), "lemnos is built into the kernel");
	check(access("/lemnos.ko", F_OK) != 0, "no lemnos.ko is in the image");
	check(!access("/dev/lemnos", R_OK), "control node exists at boot, before any daemon");

	parameter("owned", value, sizeof(value));
	check(strcmp(value, "lemnos,test-block,lemnos,test-spi") == 0,
	      "owned= from the command line is honoured");
	parameter("bus_buffer", value, sizeof(value));
	check(strcmp(value, "8192") == 0, "bus_buffer= from the command line is honoured");

	{
		char *const claim[] = { "/lemnos_claim_check", "/dev/lemnos", NULL };
		check(run(claim) == 0, "claims pass on the built-in module");
	}

	printf("lemnos-builtin: %s (%d failed)\n", failures ? "FAILED" : "passed", failures);
	sync();
	reboot(RB_POWER_OFF);
	return 0;
}
