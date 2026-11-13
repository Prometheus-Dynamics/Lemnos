#!/usr/bin/env python3
"""Judges the F2h board step: which frame-period candidate the board's means support.

Stdlib only. Usage:
  F2h-judge.py <logdir> [--bit2 0|1] [--write-logs]

<logdir> holds the output of `lemnos-rate-proof --lines N` (testing/board-runs/F2h.md, step 2):
  gated-<N>.log (board-gated.toml) and cont-<N>.log (board-cc.toml), at least three lines per mode.
  write-before.log and write-after.log: gated 2092, either side of the 0x382C/D write (step 3).

Candidates, for a frame of N lines at HTS (765 gated, 728 continuous) on the 96 MHz system clock:
  M0  period = N x HTS                     (the contract's arithmetic: intercept 0)
  A   period = (N + 1) x HTS               (intercept one line)
  B1  period = (N - 1) x HTS + 1456 sysclk (intercept (1456 - HTS) sysclk; 1456 = hts_global_tx default)
Prints PASS or FAIL for each check. Exit status 0 only when every check passes.
"""
import re
import sys
from pathlib import Path

SYSCLK_HZ = 96_000_000
NS_PER_SYSCLK = 1e9 / SYSCLK_HZ
GLOBAL_LINE_SYSCLK = 1456          # hts_global_tx, datasheet default 0x05B0 (table 7-8)
INTERCEPT_TOL_NS = 150             # a candidate's intercept must be this close
WRITE_STEP_SYSCLK = 80             # 1536 - 1456, the write in step 3
WRITE_TOL_NS = 100
MEAN_RE = re.compile(
    r"mean interval (\d+) ns over (\d+) intervals at (\d+) lines, line time (\d+) ps")
failures = []


def check(ok, text):
    print(("PASS " if ok else "FAIL ") + text)
    if not ok:
        failures.append(text)


def read_runs(path):
    m = MEAN_RE.search(path.read_text())
    if not m:
        return None
    return {"mean": int(m[1]), "lines": int(m[3]), "line_ps": int(m[4])}


def fit(points):
    n = len(points)
    sx = sum(x for x, _ in points)
    sy = sum(y for _, y in points)
    sxx = sum(x * x for x, _ in points)
    sxy = sum(x * y for x, y in points)
    slope = (n * sxy - sx * sy) / (n * sxx - sx * sx)
    return slope, (sy - slope * sx) / n


def judge_mode(logdir, mode):
    runs = sorted(logdir.glob(f"{mode}-*.log"))
    points, line_ps = [], None
    for path in runs:
        run = read_runs(path)
        if run is None:
            check(False, f"{path.name}: no 'mean interval' line in the output")
            continue
        points.append((run["lines"], run["mean"]))
        line_ps = run["line_ps"]
    if len(points) < 3:
        check(False, f"{mode}: needs at least 3 runs, has {len(points)}")
        return None
    hts = line_ps * SYSCLK_HZ / 1e12
    nominal_ns = line_ps / 1000
    slope, intercept = fit(points)
    print(f"{mode}: HTS {hts:.0f} sysclk, line {nominal_ns:.3f} ns; "
          f"slope {slope:.3f} ns/line ({(slope / nominal_ns - 1) * 1e6:+.1f} ppm); "
          f"intercept {intercept:.1f} ns = {intercept / NS_PER_SYSCLK:.1f} sysclk")
    candidates = {
        "M0": 0.0,
        "A": hts * NS_PER_SYSCLK,
        "B1": (GLOBAL_LINE_SYSCLK - hts) * NS_PER_SYSCLK,
    }
    for name, value in candidates.items():
        print(f"  {name}: intercept {value:.1f} ns, residual {intercept - value:+.1f} ns")
    close = [n for n, v in candidates.items() if abs(intercept - v) < INTERCEPT_TOL_NS]
    # Continuous: B1 and A are the same line (1456 = 2 x 728), so both match and only the gated runs
    # separate them. Gated: only B1 may match.
    wanted = ["B1"] if mode == "gated" else ["A", "B1"]
    check(close == wanted,
          f"{mode}: the intercept matches {' or '.join(wanted)} (matches {close or 'none'})"
          + ("; A and B1 coincide here, the gated runs decide" if mode == "cont" else ""))
    return close


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    logdir = Path(argv[1])
    bit2 = None
    if "--bit2" in argv:
        bit2 = argv[argv.index("--bit2") + 1]
    for mode in ("gated", "cont"):
        judge_mode(logdir, mode)
    if bit2 is not None:
        check(bit2 == "1", "0x382E bit 2 (r_tc_hts_blank) is 1 on the board "
              "(0 is the datasheet default; if 0, B1 has no register behind it)")
    before, after = logdir / "write-before.log", logdir / "write-after.log"
    if before.exists() and after.exists():
        b, a = read_runs(before), read_runs(after)
        if b is None or a is None:
            check(False, "write logs: no 'mean interval' line")
        else:
            delta = a["mean"] - b["mean"]
            expected = WRITE_STEP_SYSCLK * NS_PER_SYSCLK
            check(abs(delta - expected) < WRITE_TOL_NS,
                  f"0x382C/D write to 1536: mean moved {delta:+d} ns, expected {expected:+.1f} ns (B1 "
                  "with the global line in use); a move near 0 means the 1456 line is not hts_global_tx")
    print("F2h judge: " + ("all checks passed" if not failures else f"{len(failures)} check(s) failed"))
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
