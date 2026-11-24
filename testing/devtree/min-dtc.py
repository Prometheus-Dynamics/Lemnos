#!/usr/bin/env python3
"""min-dtc.py <overlay-dtc.toml>: prints its minimum_dtc, a dotted version; anything else is an error."""
import re
import sys
import tomllib

with open(sys.argv[1], "rb") as f:
    version = tomllib.load(f)["minimum_dtc"]
if not isinstance(version, str) or not re.fullmatch(r"[0-9]+(\.[0-9]+)+", version):
    sys.exit(f"min-dtc.py: minimum_dtc in {sys.argv[1]} is {version!r}, not a version")
print(version)
