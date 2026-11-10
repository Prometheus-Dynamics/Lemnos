// SPDX-License-Identifier: GPL-2.0
/*
 * GPIO lines through gpiolib and PWM channels through the PWM core, against
 * a simulated chip of each kind.
 *
 * Usage: lemnos_gpio_check /dev/lemnos <gpio chip number> <simulated line dir> <pwm provider>
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "../lemnos.h"

static int failures, fd;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		int ok_ = !!(cond);                                        \
		printf(ok_ ? "ok   " : "FAIL ");                           \
		failures += !ok_;                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

static int call(unsigned long cmd, void *arg)
{
	return ioctl(fd, cmd, arg) ? -errno : 0;
}

static int claim(const char *name)
{
	struct lemnos_claim c = { 0 };

	strncpy(c.resource, name, sizeof(c.resource) - 1);
	return ioctl(fd, LEMNOS_IOC_CLAIM, &c) ? -errno : c.fd;
}

/* A line of the simulated chip, by its attribute: "value" or "pull". */
static char sim[160];

static int sim_read(int line, const char *attr)
{
	char path[256], text[8] = { 0 };
	int f;

	snprintf(path, sizeof(path), "%s/sim_gpio%d/%s", sim, line, attr);
	f = open(path, O_RDONLY);
	if (f < 0)
		return -1;
	if (read(f, text, sizeof(text) - 1) <= 0)
		text[0] = 'x';
	close(f);
	return strncmp(text, "pull-up", 7) ? (text[0] == 'p' ? 0 : atoi(text)) : 1;
}

static int sim_pull(int line, const char *how)
{
	char path[256];
	int f, r;

	snprintf(path, sizeof(path), "%s/sim_gpio%d/pull", sim, line);
	f = open(path, O_WRONLY);
	if (f < 0)
		return -1;
	r = write(f, how, strlen(how)) > 0 ? 0 : -1;
	close(f);
	return r;
}

static int request(int c, uint32_t chip, uint32_t offset)
{
	struct lemnos_gpio_request r = { .claim = c, .chip = chip, .offset = offset };

	return call(LEMNOS_IOC_GPIO_REQUEST, &r);
}

static int configure(int c, uint32_t flags, uint32_t value)
{
	struct lemnos_gpio_config g = { .claim = c, .flags = flags, .value = value };

	return call(LEMNOS_IOC_GPIO_CONFIG, &g);
}

static int get(int c, uint32_t *value)
{
	struct lemnos_gpio_value v = { .claim = c };
	int r = call(LEMNOS_IOC_GPIO_GET, &v);

	*value = v.value;
	return r;
}

static int set(int c, uint32_t value)
{
	struct lemnos_gpio_value v = { .claim = c, .value = value };

	return call(LEMNOS_IOC_GPIO_SET, &v);
}

static int claim_line(uint32_t chip, uint32_t offset)
{
	char name[LEMNOS_NAME_MAX];

	snprintf(name, sizeof(name), "gpio-%u:%u", chip, offset);
	return claim(name);
}

static void check_gpio(uint32_t chip)
{
	uint32_t v = 9;
	int c = claim_line(chip, 0);

	CHECK(request(-1, chip, 0) == -EPERM, "no claim: EPERM");
	CHECK(request(c, chip, 1) == -EPERM, "a claim on another line: EPERM");
	CHECK(set(c, 1) == -EPERM, "a claim with no line requested: EPERM");
	CHECK(!request(c, chip, 0), "line requested");
	CHECK(request(c, chip, 0) == -EBUSY, "requested twice on one claim: EBUSY");

	CHECK(!configure(c, LEMNOS_GPIO_OUTPUT, 1), "output, high");
	CHECK(sim_read(0, "value") == 1, "the chip sees 1");
	CHECK(!set(c, 0) && sim_read(0, "value") == 0, "set low: the chip sees 0");
	CHECK(set(c, 2) == -EINVAL, "a value of 2: EINVAL");
	CHECK(!configure(c, LEMNOS_GPIO_OUTPUT | LEMNOS_GPIO_ACTIVE_LOW, 1) &&
		      sim_read(0, "value") == 0,
	      "active low, set 1: the chip sees 0");
	CHECK(!get(c, &v) && v == 1, "active low reads back 1");

	CHECK(!configure(c, 0, 0), "input");
	CHECK(set(c, 1) == -EPERM, "setting an input: EPERM");
	CHECK(!sim_pull(0, "pull-up") && !get(c, &v) && v == 1, "pulled up: reads 1");
	CHECK(!sim_pull(0, "pull-down") && !get(c, &v) && v == 0, "pulled down: reads 0");
	CHECK(!configure(c, LEMNOS_GPIO_PULL_UP, 0) && !get(c, &v) && v == 1, "bias pull-up: reads 1");
	CHECK(!configure(c, LEMNOS_GPIO_PULL_DOWN, 0) && !get(c, &v) && v == 0, "bias pull-down: reads 0");
	CHECK(configure(c, LEMNOS_GPIO_PULL_UP | LEMNOS_GPIO_PULL_DOWN, 0) == -EINVAL,
	      "two biases: EINVAL");
	CHECK(configure(c, LEMNOS_GPIO_OUTPUT | LEMNOS_GPIO_RISING, 0) == -EINVAL,
	      "edges on an output: EINVAL");
	CHECK(configure(c, 1u << 20, 0) == -EINVAL, "unknown flags: EINVAL");

	/* Edges: both directions, in order, stamped, and a read that blocks on nothing. */
	struct lemnos_gpio_event ev[4];
	struct pollfd p = { .fd = c, .events = POLLIN };

	CHECK(!sim_pull(0, "pull-down"), "start low");
	CHECK(!configure(c, LEMNOS_GPIO_RISING | LEMNOS_GPIO_FALLING, 0), "edges on");
	fcntl(c, F_SETFL, O_NONBLOCK);
	CHECK(read(c, ev, sizeof(ev)) < 0 && errno == EAGAIN, "none yet: EAGAIN");
	CHECK(!sim_pull(0, "pull-up") && poll(&p, 1, 1000) == 1, "rising edge seen");
	int n = (int)read(c, ev, sizeof(ev));

	CHECK(n == sizeof(ev[0]) && ev[0].rising == 1 && ev[0].ns, "rising: seq %u", ev[0].seq);
	uint32_t seq = ev[0].seq;

	CHECK(!sim_pull(0, "pull-down") && poll(&p, 1, 1000) == 1, "falling edge seen");
	n = (int)read(c, ev, sizeof(ev));
	CHECK(n == sizeof(ev[0]) && ev[0].rising == 0 && ev[0].seq == seq + 1, "falling: seq %u", ev[0].seq);
	CHECK(read(c, ev, 1) < 0 && errno == EINVAL, "a read too short: EINVAL");

	close(c);
	c = claim_line(chip, 0);
	CHECK(c >= 0 && !request(c, chip, 0), "after closing, the line can be requested again");
	close(c);

	c = claim_line(chip, 8);
	CHECK(request(c, chip, 8) == -EINVAL, "a line past the chip's end: EINVAL");
	close(c);
	c = claim_line(chip + 100, 0);
	CHECK(request(c, chip + 100, 0) == -ENODEV, "no such chip: ENODEV");
	close(c);
}

static void check_pwm(const char *provider)
{
	char name[LEMNOS_NAME_MAX];
	int c;

	snprintf(name, sizeof(name), "pwm-%s:0", provider);
	c = claim(name);
	struct lemnos_pwm_request r = { .claim = c, .channel = 0 };

	strncpy(r.provider, provider, sizeof(r.provider) - 1);
	struct lemnos_pwm_request wrong = r;

	wrong.channel = 1;
	CHECK(call(LEMNOS_IOC_PWM_REQUEST, &wrong) == -EPERM, "a claim on another channel: EPERM");
	CHECK(!call(LEMNOS_IOC_PWM_REQUEST, &r), "channel requested");

	struct lemnos_pwm_state s = {
		.claim = c, .enabled = 1, .period_ns = 1000000, .duty_ns = 250000,
	};
	struct lemnos_pwm_state back = { .claim = c };

	CHECK(!call(LEMNOS_IOC_PWM_APPLY, &s), "applied 1 ms, 25 percent");
	CHECK(!call(LEMNOS_IOC_PWM_GET, &back) && back.enabled == 1 && back.period_ns == 1000000 &&
		      back.duty_ns == 250000 && !back.inverted,
	      "state back: %llu ns, %llu ns, on %u", (unsigned long long)back.period_ns,
	      (unsigned long long)back.duty_ns, back.enabled);
	s.duty_ns = 2000000;
	CHECK(call(LEMNOS_IOC_PWM_APPLY, &s) == -EINVAL, "duty past period: EINVAL");
	s.duty_ns = 250000;
	s.enabled = 2;
	CHECK(call(LEMNOS_IOC_PWM_APPLY, &s) == -EINVAL, "enabled of 2: EINVAL");
	s.enabled = 1;
	s.claim = -1;
	CHECK(call(LEMNOS_IOC_PWM_APPLY, &s) == -EPERM, "no claim: EPERM");
	close(c);

	c = claim(name);
	r.claim = c;
	CHECK(!call(LEMNOS_IOC_PWM_REQUEST, &r), "after closing, the channel can be requested again");
	back.claim = c;
	CHECK(!call(LEMNOS_IOC_PWM_GET, &back) && back.enabled == 1, "and it is still running");
	close(c);

	snprintf(name, sizeof(name), "pwm-no-such-device:0");
	c = claim(name);
	r.claim = c;
	strcpy(r.provider, "no-such-device");
	CHECK(call(LEMNOS_IOC_PWM_REQUEST, &r) == -ENODEV, "no such provider: ENODEV");
	close(c);
}

int main(int argc, char **argv)
{
	if (argc < 5) {
		fprintf(stderr, "usage: lemnos_gpio_check /dev/lemnos <chip> <sim dir> <pwm provider>\n");
		return 2;
	}
	setvbuf(stdout, NULL, _IOLBF, 0);
	fd = open(argv[1], O_RDWR | O_CLOEXEC);
	if (fd < 0) {
		perror(argv[1]);
		return 2;
	}
	strncpy(sim, argv[3], sizeof(sim) - 1);
	check_gpio((uint32_t)atoi(argv[2]));
	check_pwm(argv[4]);
	close(fd);
	printf("%s: %d failed\n", argv[1], failures);
	return failures ? 1 : 0;
}
