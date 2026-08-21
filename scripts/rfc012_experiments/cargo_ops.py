"""Subprocess focused spaghetti-napi tests as real timed operations."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_TARGET = Path("/Volumes/SamsungRed/spaghetti-rfc012/build/w3-int/target")


def run_napi_lib_test(test_name: str, *, timeout: int = 180) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.setdefault("CARGO_TARGET_DIR", str(DEFAULT_TARGET))
    return subprocess.run(
        [
            "cargo",
            "test",
            "-p",
            "spaghetti-napi",
            "--lib",
            test_name,
        ],
        cwd=REPO,
        env=env,
        check=True,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
