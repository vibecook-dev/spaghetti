#!/usr/bin/env python3
"""Code-shape ratchet for the RFC 012 landing (docs/rfcs/012-landing-plan.md §4–5).

Fails when the codebase gets *worse* than the recorded baseline on a few
mechanical shape metrics, and never blocks improvement:

1. No crate-level `#![allow(dead_code)]` in any Rust lib.rs.
2. No production `.rs` file above PROD_CAP lines (lines before the first
   `#[cfg(test)]`). Files already above the cap are listed in the baseline and
   may only shrink.
3. No inline `mod tests` block above TEST_CAP lines; move it to `tests.rs`.
   Existing oversized blocks are baselined and may only shrink.
4. The SDK barrel (`packages/sdk/src/index.ts`) may not grow its export
   statement count beyond the baseline; public API changes are deliberate and
   shrink the number or keep it.

Usage:
  check_code_shape.py              # verify against scripts/code_shape/baseline.json
  check_code_shape.py --write-baseline   # re-record the baseline (integrator only)
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "baseline.json"
CRATE_SRC = ROOT / "crates" / "spaghetti-napi" / "src"
OTHER_CRATES = [ROOT / "crates" / "spaghetti-coverage" / "src", ROOT / "crates" / "spaghetti-architecture" / "src"]
BARREL = ROOT / "packages" / "sdk" / "src" / "index.ts"

PROD_CAP = 3000
TEST_CAP = 500

CFG_TEST = re.compile(r"^\s*#\[cfg\(test\)\]")
EXPORT_STMT = re.compile(r"^\s*export\b")


def rust_files(base: Path):
    for p in sorted(base.rglob("*.rs")):
        yield p


def split_prod_test(path: Path) -> tuple[int, int]:
    """Return (production_lines, inline_test_lines) for one Rust file."""
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    if path.name == "tests.rs":
        return 0, len(lines)
    first = None
    for i, line in enumerate(lines):
        if CFG_TEST.match(line):
            first = i
            break
    if first is None:
        return len(lines), 0
    return first, len(lines) - first


def measure() -> dict:
    prod: dict[str, int] = {}
    inline_tests: dict[str, int] = {}
    crate_allow: list[str] = []
    for base in [CRATE_SRC, *OTHER_CRATES]:
        if not base.exists():
            continue
        for p in rust_files(base):
            rel = p.relative_to(ROOT).as_posix()
            if p.name == "lib.rs" and "#![allow(dead_code)]" in p.read_text(encoding="utf-8", errors="replace"):
                crate_allow.append(rel)
            pl, tl = split_prod_test(p)
            if pl > PROD_CAP:
                prod[rel] = pl
            if tl > TEST_CAP and p.name != "tests.rs":
                inline_tests[rel] = tl
    barrel_exports = 0
    if BARREL.exists():
        barrel_exports = sum(1 for l in BARREL.read_text(encoding="utf-8").splitlines() if EXPORT_STMT.match(l))
    return {
        "prod_cap": PROD_CAP,
        "test_cap": TEST_CAP,
        "crate_level_allow_dead_code": crate_allow,
        "prod_files_over_cap": prod,
        "inline_test_blocks_over_cap": inline_tests,
        "sdk_barrel_export_statements": barrel_exports,
    }


def main(argv: list[str]) -> int:
    current = measure()
    if "--write-baseline" in argv:
        BASELINE.write_text(json.dumps(current, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"wrote {BASELINE.relative_to(ROOT)}")
        return 0
    if not BASELINE.exists():
        print("error: baseline missing; run with --write-baseline", file=sys.stderr)
        return 1
    base = json.loads(BASELINE.read_text(encoding="utf-8"))
    failures: list[str] = []

    if current["crate_level_allow_dead_code"]:
        failures.append(f"crate-level #![allow(dead_code)] in: {', '.join(current['crate_level_allow_dead_code'])}")

    for rel, n in current["prod_files_over_cap"].items():
        allowed = base["prod_files_over_cap"].get(rel)
        if allowed is None:
            failures.append(f"{rel}: {n} production lines > cap {PROD_CAP} (new oversized file)")
        elif n > allowed:
            failures.append(f"{rel}: {n} production lines > baseline {allowed} (may only shrink)")

    for rel, n in current["inline_test_blocks_over_cap"].items():
        allowed = base["inline_test_blocks_over_cap"].get(rel)
        if allowed is None:
            failures.append(f"{rel}: inline tests {n} lines > cap {TEST_CAP}; move to tests.rs")
        elif n > allowed:
            failures.append(f"{rel}: inline tests {n} lines > baseline {allowed} (may only shrink)")

    if current["sdk_barrel_export_statements"] > base["sdk_barrel_export_statements"]:
        failures.append(
            f"packages/sdk/src/index.ts: {current['sdk_barrel_export_statements']} export statements > baseline "
            f"{base['sdk_barrel_export_statements']} (public API grows only by deliberate baseline update)"
        )

    print(
        f"code shape: {len(current['prod_files_over_cap'])} prod files > {PROD_CAP} "
        f"(baseline {len(base['prod_files_over_cap'])}), "
        f"{len(current['inline_test_blocks_over_cap'])} inline test blocks > {TEST_CAP} "
        f"(baseline {len(base['inline_test_blocks_over_cap'])}), "
        f"barrel exports {current['sdk_barrel_export_statements']} (baseline {base['sdk_barrel_export_statements']})"
    )
    if failures:
        print("code shape ratchet FAILED:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("code shape ratchet OK")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
