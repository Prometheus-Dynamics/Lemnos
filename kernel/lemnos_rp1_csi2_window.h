/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * The stop window of the rp1-csi2 receiver: how long the channel's line
 * counter must hold still before its DMA is taken as stopped. Plain C, so the
 * driver and its check compute it the same way.
 *
 * The assumption behind it: while the channel writes, its line counter moves at
 * least once per line time. A stop is done when the counter has held still for
 * a window longer than the longest line that can be in flight. The window is
 * three line times, and never less than LRC_WINDOW_MIN_US; it is counted in
 * reads, each at least LRC_WINDOW_READ_GAP_US apart, so the reads needed are the
 * window over that gap, rounded up. The reads are capped at LRC_STOP_STILL_MAX:
 * a line whose window would need more than that cannot be waited for, and START
 * refuses it. Not measured on a board: testing/board-runs/FLAKE-CSI2.md.
 */
#ifndef LEMNOS_RP1_CSI2_WINDOW_H
#define LEMNOS_RP1_CSI2_WINDOW_H

#define LRC_WINDOW_MIN_US	1600	/* 8 reads at the shortest gap */
#define LRC_WINDOW_READ_GAP_US	200
#define LRC_WINDOW_LINE_MULT	3
#define LRC_STOP_STILL_MAX	4096	/* reads: a window of about 0.8 s at most */

/* Time for one line of `bytes` on `lanes` lanes at `mbps` each, rounded up, in us. Lanes and rate >= 1. */
static inline unsigned long long lrc_line_us(unsigned long long bytes, unsigned int lanes,
					     unsigned int mbps)
{
	unsigned long long rate = (unsigned long long)lanes * mbps;

	return (bytes * 8 + rate - 1) / rate;
}

/* Whether a line of `line_us` has a window the stop can wait for. */
static inline int lrc_window_fits(unsigned long long line_us)
{
	unsigned long long window = LRC_WINDOW_LINE_MULT * line_us;

	if (window < LRC_WINDOW_MIN_US)
		window = LRC_WINDOW_MIN_US;
	return (window + LRC_WINDOW_READ_GAP_US - 1) / LRC_WINDOW_READ_GAP_US <= LRC_STOP_STILL_MAX;
}

/* The reads that must be still for a stop to be done, for a line of `line_us`. Assumes lrc_window_fits. */
static inline unsigned int lrc_still_reads(unsigned long long line_us)
{
	unsigned long long window = LRC_WINDOW_LINE_MULT * line_us;

	if (window < LRC_WINDOW_MIN_US)
		window = LRC_WINDOW_MIN_US;
	return (window + LRC_WINDOW_READ_GAP_US - 1) / LRC_WINDOW_READ_GAP_US;
}

#endif
