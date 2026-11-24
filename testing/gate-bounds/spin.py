#!/usr/bin/env python3
"""spin.py [seconds]: keeps one CPU busy for that many seconds of wall time, or for ever without one."""
import sys
import time

if len(sys.argv) > 1:
    until = time.time() + float(sys.argv[1])
    while time.time() < until:
        pass
else:
    while True:
        pass
