#!/bin/bash
set -euo pipefail

# Pick a Python 3 interpreter.
#
# Plain `python3` is not portable to Windows: a stock install exposes a
# `python3.exe` App Execution Alias that prints "Python was not found..."
# and *exits 0*, so neither `command -v` nor the exit status can tell it
# apart from a real interpreter. Probe by output instead, and accept the
# `python` / `py` spellings that Windows actually ships.
PYTHON=""
for candidate in python3 python py; do
  if [ "$("$candidate" -c 'import sys; print(sys.version_info[0])' 2>/dev/null)" = "3" ]; then
    PYTHON="$candidate"
    break
  fi
done

if [ -z "$PYTHON" ]; then
  echo "error: no Python 3 interpreter found (tried: python3, python, py)" >&2
  echo "       install Python 3 and re-run, or run 'pnpm test:packages' to skip the validators" >&2
  exit 1
fi

# The validators print box-drawing characters and read TS sources containing
# em dashes. Python picks its stdio and default file encoding from the console,
# which on Windows is cp1252 — so both the output and the reads die with
# UnicodeEncodeError/UnicodeDecodeError outside a UTF-8 terminal. UTF-8 mode
# fixes both, and belongs here rather than in the CI workflow so it also covers
# anyone running this from cmd.exe or PowerShell.
export PYTHONUTF8=1
export PYTHONIOENCODING=utf-8

echo "Running spaghetti type validators... (using $PYTHON)"
echo ""

PASS=0
FAIL=0
SKIP=0

# Exit 78 means "no real Claude Code data here" — the type lookups still ran at
# import, so a renamed or deleted interface has already failed. Only the
# real-data half is unavailable, which is the normal case on a CI runner.
SKIP_CODE=78

run_suite() {
  local title="$1" status=0
  shift
  echo "=== $title ==="
  "$PYTHON" "$@" 2>&1 | tail -3 || status=$?
  case "$status" in
    0) PASS=$((PASS+1)) ;;
    "$SKIP_CODE") SKIP=$((SKIP+1)) ;;
    *) FAIL=$((FAIL+1)) ;;
  esac
  echo ""
}

run_suite "Session/Message Types" scripts/validate_sessions_and_messages.py
run_suite "Config/Settings Types" scripts/validate_config_and_settings.py
run_suite "Secondary Data Types" scripts/validate_secondary_data.py
run_suite "RFC 011 Architecture Boundaries" scripts/architecture/check_rfc011_boundaries.py
run_suite "RFC 012 / RFC 011 Compatibility Ledger" scripts/architecture/check_rfc012_delta.py
run_suite "RFC 012A Agent Support Contracts" scripts/agent_support/validate.py
run_suite "RFC 012A Agent Support Tooling" -m unittest scripts.agent_support.test_contracts
run_suite "RFC 012C Usage-v2 Oracle" scripts/usage_v2_oracle/test_oracle.py

echo "=============================="
echo "Validation suites: $PASS passed, $FAIL failed, $SKIP skipped"
if [ $SKIP -gt 0 ]; then
  echo ""
  echo "Skipped suites need a real ~/.claude. They only catch Claude Code"
  echo "changing its on-disk format, which no CI runner can observe — run"
  echo "'pnpm validate' on a machine you actually use Claude Code on."
fi
if [ $FAIL -gt 0 ]; then
  exit 1
fi
