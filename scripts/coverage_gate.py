#!/usr/bin/env python3
"""Enforce the per-crate coverage floor from TESTING_STRATEGY.md.

`cargo llvm-cov --summary-only` reports per *file*; the documented gate is
per *crate*, so the rows are rolled up here. Exits non-zero naming every
crate below the floor, rather than only the first.
"""
import collections
import re
import sys

FLOOR = 85.0
# TESTING_STRATEGY.md: ">=85% on `sandesh-core` and protocol crates".
GATED = ("sandesh-core", "sandesh-proto", "sandesh-smtp", "sandesh-imap", "sandesh-pop3")


def main() -> int:
    totals = collections.defaultdict(lambda: [0, 0])
    for line in sys.stdin:
        if not line.startswith("crates/"):
            continue
        parts = line.split()
        if len(parts) < 10:
            continue
        try:
            lines, missed = int(parts[7]), int(parts[8])
        except ValueError:
            continue
        crate = re.match(r"crates/([^/]+)/", parts[0])
        if crate:
            totals[crate.group(1)][0] += lines
            totals[crate.group(1)][1] += missed

    if not totals:
        print("coverage gate: no crate rows parsed — did the report format change?")
        return 1

    failed = []
    for crate in GATED:
        lines, missed = totals.get(crate, [0, 0])
        if lines == 0:
            print(f"coverage gate: {crate} produced no rows")
            return 1
        pct = 100.0 * (lines - missed) / lines
        status = "ok" if pct >= FLOOR else "BELOW FLOOR"
        print(f"  {crate:<16} {pct:6.2f}%  {status}")
        if pct < FLOOR:
            failed.append((crate, pct))

    if failed:
        print()
        for crate, pct in failed:
            print(f"coverage gate FAILED: {crate} at {pct:.2f}%, floor is {FLOOR}%")
        return 1

    print(f"\ncoverage gate passed: every gated crate is at or above {FLOOR}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
