#!/usr/bin/env python3
"""Reads lemnos-ctl watch lines on stdin and prints the IMU rows of the reference set.

  lemnos-ctl watch imu acceleration --period-ms 5 --count 9000 \
    | python3 -I stamp.py --period-ms 5 --expect 9000

A line is one delivered reading, stamped with the monotonic clock when it is read. A miss is a gap
longer than 1.5 periods. A repeat is a reading equal to the one before it. Rows are TSV: key,
status, value, note. Nothing is made up: with no readings, the rows say not measured.
"""

import argparse
import sys
import time


def row(key, status, value, note):
    print(f"{key}\t{status}\t{value}\t{note}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--period-ms", type=float, required=True)
    parser.add_argument("--expect", type=int, required=True)
    args = parser.parse_args()

    stamps = []
    values = []
    for line in sys.stdin.buffer:
        stamps.append(time.monotonic_ns())
        values.append(line)

    note = f"board, lemnos-ctl watch at {args.period_ms:g} ms, {args.expect} requested"
    if not values:
        for key in ("imu.delivered", "imu.misses", "imu.largest_gap_ms", "imu.repeats"):
            row(key, "not_measured", "", "no readings from the IMU")
        return

    gaps = [(later - earlier) / 1e6 for earlier, later in zip(stamps, stamps[1:])]
    misses = sum(1 for gap in gaps if gap > 1.5 * args.period_ms)
    repeats = sum(1 for before, after in zip(values, values[1:]) if before == after)
    largest = max(gaps, default=0.0)
    row("imu.delivered", "measured", f"{len(values)} of {args.expect}", note)
    row("imu.misses", "measured", str(misses), f"gaps over {1.5 * args.period_ms:g} ms")
    row("imu.largest_gap_ms", "measured", f"{largest:.2f}", note)
    row("imu.repeats", "measured", str(repeats), "readings equal to the one before")


main()
