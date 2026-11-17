/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * The line time, in microseconds: the one thing the stop bound and the check need from this header.
 */
#ifndef LEMNOS_RP1_CSI2_WINDOW_H
#define LEMNOS_RP1_CSI2_WINDOW_H

/* Time for one line of `bytes` on `lanes` lanes at `mbps` each, rounded up, in us. Lanes and rate >= 1. */
static inline unsigned long long lrc_line_us(unsigned long long bytes, unsigned int lanes,
					     unsigned int mbps)
{
	unsigned long long rate = (unsigned long long)lanes * mbps;

	return (bytes * 8 + rate - 1) / rate;
}

#endif
