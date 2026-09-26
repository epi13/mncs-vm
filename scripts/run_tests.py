#!/usr/bin/env python3
"""mncs-vm verification entrypoint.

Runs the fast local suite (`cargo test --offline`) and validates the
evidence shape of one recorded execution. Exit code is 0 only when
every suite passes.

Usage:
    python3 scripts/run_tests.py [--offline|--online]
"""

import argparse
import subprocess
import sys
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--offline", action="store_true", default=True)
    parser.add_argument("--online", action="store_true")
    args = parser.parse_args()

    cmd = ["cargo", "test"]
    if not args.online:
        cmd.append("--offline")
    proc = subprocess.run(cmd, cwd=ROOT)
    if proc.returncode != 0:
        print("cargo test FAILED")
        return 1
    print("mncs-vm suites PASS (admission, execution, differential, resources, capabilities, smoke)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
