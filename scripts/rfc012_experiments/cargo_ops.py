"""Subprocess focused spaghetti-napi tests as real timed operations."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def run_napi_lib_test(
    test_name: str,
    *,
    timeout: int = 180,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
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
