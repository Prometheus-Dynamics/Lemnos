#!/usr/bin/env python3
"""segments.py: reads "<VirtAddr> <MemSiz>" lines (hex) on stdin, one per LOAD segment, and prints
the lowest start and the highest end (start + size), in hex. No segment is an error."""
import sys

rows = [line.split() for line in sys.stdin if line.strip()]
if not rows or any(len(r) != 2 for r in rows):
    sys.exit(f"segments.py: expected one or more '<VirtAddr> <MemSiz>' lines, read {rows!r}")
starts = [int(r[0], 16) for r in rows]
ends = [int(r[0], 16) + int(r[1], 16) for r in rows]
print(hex(min(starts)), hex(max(ends)))
