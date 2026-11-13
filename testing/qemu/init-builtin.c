// SPDX-License-Identifier: GPL-2.0
/*
 * The guest's first process for a kernel with no loadable modules, lemnos built in. It checks
 * what the kernel command line says and what the built-in module did with it: each parameter
 * reads back what the command line gave it (or its default), the control node exists before any
 * daemon starts, module loading is refused, and the claim checks pass. Prints one result line and
 * powers off. Static, so the image needs nothing else.
 *
 * What this proves: the module is built in and initialised at boot, its parameters come from the
 * command line, and the control node and claims work. It does not drive any block: no fake
 * device is in the kernel, so nothing here reaches a bus, a stream or a DMA engine.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int failures;

static void check(int ok, const char *what)
{
	printf("%s %s\n", ok ? "ok  " : "FAIL", what);
	failures += !ok;
}

/* The value a parameter read back as, or "" when it cannot be read. */
static void read_parameter(const char *name, char *buf, size_t size)
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

/* The value the command line gave `name`, as lemnos.name=value, or `fallback` when it gave none. */
static void command_line_value(const char *name, const char *fallback, char *buf, size_t size)
{
	char cmdline[2048], key[64], *token;
	FILE *f = fopen("/proc/cmdline", "r");

	snprintf(key, sizeof(key), "lemnos.%s=", name);
	buf[0] = '\0';
	if (f) {
		if (fgets(cmdline, sizeof(cmdline), f))
			cmdline[strcspn(cmdline, "\n")] = '\0';
		fclose(f);
		for (token = strtok(cmdline, " "); token; token = strtok(NULL, " ")) {
			if (!strncmp(token, key, strlen(key))) {
				snprintf(buf, size, "%s", token + strlen(key));
				return;
			}
		}
	}
	snprintf(buf, size, "%s", fallback);
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

struct parameter {
	const char *name;
	const char *fallback;
};

/* Every parameter of the module, with the default it has when the command line gives none. */
static const struct parameter parameters[] = {
	{ "owned", "" },	     { "confined", "" },	{ "unconfined", "" },
	{ "pisp_be", "" },	     { "gsdma", "" },		{ "a733_ve", "" },
	{ "vip9000", "" },	     { "k230_isp", "" },	{ "bus_buffer", "4096" },
};

int main(void)
{
	char want[256], got[256], what[160];
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
	/* With CONFIG_MODULES off, the kernel has no init_module: it answers ENOSYS. */
	errno = 0;
	check(syscall(SYS_init_module, NULL, 0UL, "") == -1 && errno == ENOSYS,
	      "the kernel refuses to load a module (no module support)");
	check(!access("/dev/lemnos", R_OK), "control node exists at boot, before any daemon");

	for (size_t i = 0; i < sizeof(parameters) / sizeof(parameters[0]); i++) {
		const struct parameter *p = &parameters[i];

		command_line_value(p->name, p->fallback, want, sizeof(want));
		read_parameter(p->name, got, sizeof(got));
		if (want[0])
			snprintf(what, sizeof(what), "%s=%s from the command line", p->name, want);
		else
			snprintf(what, sizeof(what), "%s takes its default \"%s\"", p->name, p->fallback);
		check(!strcmp(got, want), what);
	}

	{
		char *const claim[] = { "/lemnos_claim_check", "/dev/lemnos", NULL };
		check(run(claim) == 0, "claims pass on the built-in module");
	}

	printf("lemnos-builtin: %s (%d failed)\n", failures ? "FAILED" : "passed", failures);
	sync();
	reboot(RB_POWER_OFF);
	return 0;
}
