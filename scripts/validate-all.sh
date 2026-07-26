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

echo "Running spaghetti type validators... (using $PYTHON)"
echo ""

PASS=0
FAIL=0

echo "=== Session/Message Types ==="
if "$PYTHON" scripts/validate_sessions_and_messages.py 2>&1 | tail -3; then
  PASS=$((PASS+1))
else
  FAIL=$((FAIL+1))
fi

echo ""
echo "=== Config/Settings Types ==="
if "$PYTHON" scripts/validate_config_and_settings.py 2>&1 | tail -3; then
  PASS=$((PASS+1))
else
  FAIL=$((FAIL+1))
fi

echo ""
echo "=== Secondary Data Types ==="
if "$PYTHON" scripts/validate_secondary_data.py 2>&1 | tail -3; then
  PASS=$((PASS+1))
else
  FAIL=$((FAIL+1))
fi

echo ""
echo "=============================="
echo "Validation suites: $PASS passed, $FAIL failed"
if [ $FAIL -gt 0 ]; then
  exit 1
fi
