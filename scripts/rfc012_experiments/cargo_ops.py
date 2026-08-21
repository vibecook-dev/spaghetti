"""Subprocess focused spaghetti-napi tests as real timed operations."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SSD_TARGET = Path("/Volumes/SamsungRed/spaghetti-rfc012/build/w5-fix/target")


def run_napi_lib_test(
    test_name: str,
    *,
    timeout: int = 180,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    if "CARGO_TARGET_DIR" not in env and SSD_TARGET.parent.is_dir():
        env["CARGO_TARGET_DIR"] = str(SSD_TARGET)
    if extra_env:
        env.update(extra_env)
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
