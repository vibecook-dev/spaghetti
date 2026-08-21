#!/usr/bin/env python3
"""
Validate @spaghetti/core types for secondary data directories against
REAL data in ~/.claude.

Validates:
  1.  todos/           — TodoItem keys + status values + filename regex
  2.  tasks/           — directory structure, .lock, .highwatermark
  3.  file-history/    — {hash}@v{N} filename pattern
  4.  debug/           — log line regex + log levels
  5.  shell-snapshots/ — snapshot-{shell}-{timestamp}-{random}.sh
  6.  paste-cache/     — {hexHash}.txt
  7.  session-env/     — directory structure
  8.  ide/             — .lock JSON keys vs IdeLockFile type
  9.  sessions/        — ActiveSessionFile keys: {pid, sessionId, cwd, startedAt}
  10. cache/           — list all files
  11. subagent meta.json — SubagentMeta keys: {agentType, description}
"""

import json
import os
import re
import sys
from collections import defaultdict
from pathlib import Path

from ts_types import interface_fields

CLAUDE_DIR = Path.home() / ".claude"


def is_ignored_entry(name: str) -> bool:
    return name.startswith(".")

# ═══════════════════════════════════════════════════════════════════════════════
# REPORT COLLECTOR
# ═══════════════════════════════════════════════════════════════════════════════

class Section:
    def __init__(self, name: str):
        self.name = name
        self.stats: dict[str, object] = {}
        self.gaps: list[str] = []
        self.infos: list[str] = []
        self.passed = True

    def stat(self, key: str, value: object):
        self.stats[key] = value

    def gap(self, msg: str):
        self.gaps.append(msg)
        self.passed = False

    def info(self, msg: str):
        self.infos.append(msg)

    def print_report(self):
        result = "PASS" if self.passed else "FAIL"
        header = f"[{result}] {self.name}"
        print()
        print(f"{'─' * 78}")
        print(f"  {header}")
        print(f"{'─' * 78}")
        for k, v in self.stats.items():
            print(f"    {k}: {v}")
        if self.infos:
            for info in self.infos:
                print(f"    [INFO] {info}")
        if self.gaps:
            for g in self.gaps[:30]:
                print(f"    [GAP]  {g}")
            if len(self.gaps) > 30:
                print(f"    ... and {len(self.gaps) - 30} more gaps")
        print()


# ═══════════════════════════════════════════════════════════════════════════════
# 1. TODOS
# ═══════════════════════════════════════════════════════════════════════════════

def validate_todos() -> Section:
    s = Section("1. todos/  —  TodoItem type")
    todos_dir = CLAUDE_DIR / "todos"

    if not todos_dir.exists():
        s.gap("Directory does not exist")
        return s

    FILENAME_RE = re.compile(r'^(.+?)-agent-(.+)\.json$')
    EXPECTED_FIELDS = {"content", "status", "activeForm"}
    KNOWN_STATUSES = {"pending", "in_progress", "completed"}

    file_count = 0
    total_items = 0
    all_keys: set[str] = set()
    all_statuses: set[str] = set()
    unmatched_filenames: list[str] = []

    for f in sorted(os.listdir(todos_dir)):
        fpath = todos_dir / f
        if not fpath.is_file():
            continue

        file_count += 1

        # filename regex
        if not FILENAME_RE.match(f):
            unmatched_filenames.append(f)

        # parse JSON
        try:
            with open(fpath, "r") as fp:
                content = fp.read().strip()
            if not content:
                continue
            data = json.loads(content)
        except (IOError, json.JSONDecodeError):
            s.gap(f"Cannot parse {f}")
            continue

        if not isinstance(data, list):
            s.gap(f"Root is not array in {f}")
            continue

        for item in data:
            if not isinstance(item, dict):
                continue
            total_items += 1
            all_keys.update(item.keys())
            status = item.get("status")
            if status:
                all_statuses.add(status)

    s.stat("Files", file_count)
    s.stat("Total todo items", total_items)
    s.stat("All unique keys", sorted(all_keys))
    s.stat("All unique statuses", sorted(all_statuses))

    # Compare keys
    extra_keys = all_keys - EXPECTED_FIELDS
    if extra_keys:
        s.gap(f"EXTRA keys not in TodoItem type: {sorted(extra_keys)}")

    missing_keys = EXPECTED_FIELDS - all_keys
    if missing_keys:
        s.info(f"Keys in type but never seen in data (may be optional): {sorted(missing_keys)}")

    # Compare statuses
    extra_statuses = all_statuses - KNOWN_STATUSES
    if extra_statuses:
        s.gap(f"EXTRA status values: {sorted(extra_statuses)}")

    # Filename regex
    if unmatched_filenames:
        s.gap(f"{len(unmatched_filenames)} files DON'T match regex ^(.+?)-agent-(.+)\\.json$:")
        for fn in unmatched_filenames[:10]:
            s.gap(f"  {fn}")
    else:
        s.info(f"All {file_count} filenames match regex OK")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 2. TASKS
# ═══════════════════════════════════════════════════════════════════════════════

def validate_tasks() -> Section:
    s = Section("2. tasks/  —  TaskEntry type")
    tasks_dir = CLAUDE_DIR / "tasks"

    if not tasks_dir.exists():
        s.gap("Directory does not exist")
        return s

    # Task directories contain .lock, .highwatermark, and numbered task item JSON files ({N}.json)
    EXPECTED_STATIC_FILES = {".lock", ".highwatermark"}
    TASK_ITEM_RE = re.compile(r'^\d+\.json$')

    task_count = 0
    unexpected_files: list[str] = []
    lock_nonempty: list[str] = []
    hwm_non_int: list[str] = []
    hwm_values: list[int] = []

    for entry in sorted(os.listdir(tasks_dir)):
        if is_ignored_entry(entry):
            continue
        entry_path = tasks_dir / entry
        if not entry_path.is_dir():
            s.gap(f"Unexpected top-level file: {entry}")
            continue

        task_count += 1
        contents = set(os.listdir(entry_path))
        extra = {f for f in contents - EXPECTED_STATIC_FILES if not TASK_ITEM_RE.match(f)}
        if extra:
            unexpected_files.append(f"{entry}: {extra}")

        # .lock
        lock_path = entry_path / ".lock"
        if lock_path.exists():
            size = os.path.getsize(lock_path)
            if size > 0:
                lock_nonempty.append(entry)

        # .highwatermark
        hwm_path = entry_path / ".highwatermark"
        if hwm_path.exists():
            try:
                with open(hwm_path, "r") as f:
                    val = f.read().strip()
                if val:
                    try:
                        hwm_values.append(int(val))
                    except ValueError:
                        hwm_non_int.append(f"{entry}: {val!r}")
            except IOError:
                pass

    s.stat("Task directories", task_count)
    s.stat("Highwatermark value range", f"{min(hwm_values)}-{max(hwm_values)}" if hwm_values else "none")

    if unexpected_files:
        s.gap(f"Unexpected files beyond .lock/.highwatermark:")
        for uf in unexpected_files[:10]:
            s.gap(f"  {uf}")

    if lock_nonempty:
        s.gap(f"{len(lock_nonempty)} .lock files are non-empty")

    if hwm_non_int:
        s.gap(f"Non-integer highwatermark values:")
        for v in hwm_non_int[:10]:
            s.gap(f"  {v}")

    if not unexpected_files and not lock_nonempty and not hwm_non_int:
        s.info("All task directories match expected structure")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 3. FILE-HISTORY
# ═══════════════════════════════════════════════════════════════════════════════

def validate_file_history() -> Section:
    s = Section("3. file-history/  —  FileHistorySnapshotFile type")
    fh_dir = CLAUDE_DIR / "file-history"

    if not fh_dir.exists():
        s.gap("Directory does not exist")
        return s

    VERSION_RE = re.compile(r'^([0-9a-f]+)@v(\d+)$')

    session_count = 0
    file_count = 0
    hash_lengths: set[int] = set()
    unmatched: list[str] = []

    for entry in sorted(os.listdir(fh_dir)):
        if is_ignored_entry(entry):
            continue
        entry_path = fh_dir / entry
        if not entry_path.is_dir():
            s.gap(f"Top-level non-directory: {entry}")
            continue

        session_count += 1

        for item in os.listdir(entry_path):
            item_path = entry_path / item
            if item_path.is_dir():
                s.gap(f"Unexpected subdirectory: {entry}/{item}")
                continue

            file_count += 1
            m = VERSION_RE.match(item)
            if not m:
                unmatched.append(f"{entry}/{item}")
            else:
                hash_lengths.add(len(m.group(1)))

    s.stat("Sessions with history", session_count)
    s.stat("Total snapshots", file_count)
    s.stat("Hash lengths seen", sorted(hash_lengths))

    if unmatched:
        s.gap(f"{len(unmatched)} files don't match {{hash}}@v{{N}} pattern:")
        for u in unmatched[:10]:
            s.gap(f"  {u}")
    else:
        s.info(f"All {file_count} files match pattern OK")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 4. DEBUG
# ═══════════════════════════════════════════════════════════════════════════════

def validate_debug() -> Section:
    s = Section("4. debug/  —  DebugLogEntry regex + DebugLogLevel")
    debug_dir = CLAUDE_DIR / "debug"

    if not debug_dir.exists():
        s.gap("Directory does not exist")
        return s

    LOG_LINE_RE = re.compile(r'^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z \[(\w+)\] (.*)')
    STACK_LINE_RE = re.compile(r'^\s+at ')
    CONTINUATION_RE = re.compile(r'^\s+')
    KNOWN_LEVELS = {"DEBUG", "ERROR", "WARN", "INFO"}

    file_count = 0
    total_lines = 0
    matched_lines = 0
    stack_lines = 0
    continuation_lines = 0
    empty_lines = 0
    unmatched_lines = 0
    all_levels: set[str] = set()
    sample_unmatched: list[str] = []

    entries = sorted(os.listdir(debug_dir))
    # Sample: read at most 5 files fully
    txt_files = [e for e in entries if e.endswith(".txt") and not os.path.islink(debug_dir / e)]
    sample_files = txt_files[:5]

    for entry in entries:
        fpath = debug_dir / entry
        if entry == "latest" or fpath.is_dir():
            continue
        file_count += 1

    for entry in sample_files:
        fpath = debug_dir / entry
        try:
            with open(fpath, "r", errors="replace") as fp:
                for line in fp:
                    total_lines += 1
                    stripped = line.rstrip()
                    if not stripped:
                        empty_lines += 1
                        continue

                    m = LOG_LINE_RE.match(stripped)
                    if m:
                        matched_lines += 1
                        all_levels.add(m.group(1))
                    elif STACK_LINE_RE.match(stripped):
                        stack_lines += 1
                    elif CONTINUATION_RE.match(stripped):
                        continuation_lines += 1
                    else:
                        unmatched_lines += 1
                        if len(sample_unmatched) < 5:
                            sample_unmatched.append(stripped[:120])
        except IOError:
            pass

    non_empty = total_lines - empty_lines
    match_rate = (matched_lines / non_empty * 100) if non_empty > 0 else 0

    s.stat("Total debug log files", file_count)
    s.stat("Files sampled", len(sample_files))
    s.stat("Total lines sampled", f"{total_lines:,}")
    s.stat("Log entry lines (regex match)", f"{matched_lines:,}")
    s.stat("Stack trace lines", f"{stack_lines:,}")
    s.stat("Continuation lines", f"{continuation_lines:,}")
    s.stat("Empty lines", f"{empty_lines:,}")
    s.stat("Unmatched lines", f"{unmatched_lines:,}")
    s.stat("Match rate (log lines / non-empty)", f"{match_rate:.1f}%")
    s.stat("All unique log levels", sorted(all_levels))

    # Check levels
    extra_levels = all_levels - KNOWN_LEVELS
    if extra_levels:
        s.gap(f"EXTRA log levels not in DebugLogLevel: {sorted(extra_levels)}")

    if sample_unmatched:
        s.info("Sample unmatched lines (subprocess output / raw text):")
        for line in sample_unmatched:
            s.info(f"  {line}")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 5. SHELL-SNAPSHOTS
# ═══════════════════════════════════════════════════════════════════════════════

def validate_shell_snapshots() -> Section:
    s = Section("5. shell-snapshots/  —  ShellSnapshotFile type")
    ss_dir = CLAUDE_DIR / "shell-snapshots"

    if not ss_dir.exists():
        s.gap("Directory does not exist")
        return s

    PATTERN_RE = re.compile(r'^snapshot-([a-z]+)-(\d+)-([a-z0-9]+)\.sh$')

    file_count = 0
    shells: set[str] = set()
    hash_lengths: set[int] = set()
    unmatched: list[str] = []

    for entry in sorted(os.listdir(ss_dir)):
        fpath = ss_dir / entry
        if fpath.is_dir():
            s.gap(f"Unexpected directory: {entry}")
            continue

        file_count += 1
        m = PATTERN_RE.match(entry)
        if not m:
            unmatched.append(entry)
        else:
            shells.add(m.group(1))
            hash_lengths.add(len(m.group(3)))

    s.stat("Snapshot files", file_count)
    s.stat("Shell types", sorted(shells))
    s.stat("Hash lengths", sorted(hash_lengths))

    if unmatched:
        s.gap(f"{len(unmatched)} files don't match snapshot-{{shell}}-{{ts}}-{{hash}}.sh:")
        for u in unmatched[:10]:
            s.gap(f"  {u}")
    else:
        s.info(f"All {file_count} filenames match pattern OK")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 6. PASTE-CACHE
# ═══════════════════════════════════════════════════════════════════════════════

def validate_paste_cache() -> Section:
    s = Section("6. paste-cache/  —  PasteCacheFile type")
    pc_dir = CLAUDE_DIR / "paste-cache"

    if not pc_dir.exists():
        s.gap("Directory does not exist")
        return s

    HASH_RE = re.compile(r'^[0-9a-f]+\.txt$')

    file_count = 0
    hash_lengths: set[int] = set()
    unmatched: list[str] = []

    for entry in sorted(os.listdir(pc_dir)):
        fpath = pc_dir / entry
        if fpath.is_dir():
            s.gap(f"Unexpected directory: {entry}")
            continue

        file_count += 1
        m = HASH_RE.match(entry)
        if not m:
            unmatched.append(entry)
        else:
            stem = entry.rsplit(".", 1)[0]
            hash_lengths.add(len(stem))

    s.stat("Paste cache files", file_count)
    s.stat("Hash lengths", sorted(hash_lengths))

    if unmatched:
        s.gap(f"{len(unmatched)} files don't match {{hexHash}}.txt:")
        for u in unmatched[:10]:
            s.gap(f"  {u}")
    else:
        s.info(f"All {file_count} filenames match pattern OK")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 7. SESSION-ENV
# ═══════════════════════════════════════════════════════════════════════════════

def validate_session_env() -> Section:
    s = Section("7. session-env/  —  SessionEnvEntry type")
    se_dir = CLAUDE_DIR / "session-env"

    if not se_dir.exists():
        s.gap("Directory does not exist")
        return s

    UUID_RE = re.compile(r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')

    dir_count = 0
    empty_count = 0
    non_empty_count = 0
    file_patterns_found: list[str] = []

    for entry in sorted(os.listdir(se_dir)):
        if is_ignored_entry(entry):
            continue
        fpath = se_dir / entry
        if not fpath.is_dir():
            s.gap(f"Non-directory entry: {entry}")
            continue

        dir_count += 1
        if not UUID_RE.match(entry):
            s.gap(f"Directory name is not a valid UUID: {entry}")

        contents = os.listdir(fpath)
        if len(contents) == 0:
            empty_count += 1
        else:
            non_empty_count += 1
            for item in contents:
                file_patterns_found.append(item)

    s.stat("Directories", dir_count)
    s.stat("Empty dirs", empty_count)
    s.stat("Non-empty dirs", non_empty_count)

    if file_patterns_found:
        unique_patterns = set(file_patterns_found)
        s.stat("File naming patterns found", sorted(unique_patterns)[:20])
        s.info(f"Non-empty dirs contain shell scripts or other files")
    else:
        s.info("All directories are empty (as expected)")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 8. IDE
# ═══════════════════════════════════════════════════════════════════════════════

def validate_ide() -> Section:
    s = Section("8. ide/  —  IdeLockFile type")
    ide_dir = CLAUDE_DIR / "ide"

    if not ide_dir.exists():
        s.gap("Directory does not exist")
        return s

    EXPECTED_KEYS = {"workspaceFolders", "pid", "ideName", "transport", "runningInWindows", "authToken"}
    FILENAME_RE = re.compile(r'^(\d+)\.lock$')

    file_count = 0
    all_keys: set[str] = set()
    per_file_issues: list[str] = []

    for entry in sorted(os.listdir(ide_dir)):
        fpath = ide_dir / entry
        if fpath.is_dir():
            s.gap(f"Unexpected directory: {entry}")
            continue

        file_count += 1

        if not FILENAME_RE.match(entry):
            s.gap(f"Filename doesn't match {{pid}}.lock: {entry}")
            continue

        size = os.path.getsize(fpath)
        if size == 0:
            per_file_issues.append(f"{entry}: empty")
            continue

        try:
            with open(fpath) as f:
                data = json.load(f)
        except json.JSONDecodeError as e:
            per_file_issues.append(f"{entry}: invalid JSON: {e}")
            continue

        if not isinstance(data, dict):
            per_file_issues.append(f"{entry}: not an object")
            continue

        actual = set(data.keys())
        all_keys.update(actual)

        extra = actual - EXPECTED_KEYS
        if extra:
            per_file_issues.append(f"{entry}: EXTRA keys: {extra}")

        missing = EXPECTED_KEYS - actual
        if missing:
            per_file_issues.append(f"{entry}: missing keys: {missing}")

    s.stat("IDE lock files", file_count)
    s.stat("All unique keys collected", sorted(all_keys))

    extra_overall = all_keys - EXPECTED_KEYS
    if extra_overall:
        s.gap(f"EXTRA keys not in IdeLockFile type: {sorted(extra_overall)}")

    if per_file_issues:
        for issue in per_file_issues[:15]:
            s.gap(issue)

    if not extra_overall and not per_file_issues:
        s.info("All lock files match IdeLockFile type perfectly")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 9. SESSIONS (active sessions)
# ═══════════════════════════════════════════════════════════════════════════════

def validate_sessions() -> Section:
    s = Section("9. sessions/  —  ActiveSessionFile type")
    sessions_dir = CLAUDE_DIR / "sessions"

    if not sessions_dir.exists():
        s.gap("Directory does not exist")
        return s

    # Read straight from the interface. The hand-written copy this replaced
    # listed only {kind, entrypoint, name} as optional, so the eight fields a
    # 2026-07 audit had already added to ActiveSessionFile were reported as
    # unmodelled for as long as the copy went unmaintained.
    REQUIRED_KEYS = {"pid", "sessionId", "cwd", "startedAt"}
    EXPECTED_KEYS = interface_fields("ActiveSessionFile", "claude/toplevel-files-data.ts")

    file_count = 0
    key_file_count = 0
    all_keys: set[str] = set()
    per_file_issues: list[str] = []

    for entry in sorted(os.listdir(sessions_dir)):
        if is_ignored_entry(entry):
            continue
        fpath = sessions_dir / entry
        if fpath.is_dir():
            s.gap(f"Unexpected directory: {entry}")
            continue
        if not entry.endswith(".json"):
            key_match = re.fullmatch(r"(\d+)\.([0-9a-f]{64})\.key", entry)
            if key_match:
                key_file_count += 1
                if fpath.stat().st_size == 0:
                    s.gap(f"Empty active-session key sidecar: {entry}")
                continue
            s.gap(f"Unknown active-session sidecar: {entry}")
            continue

        file_count += 1

        try:
            with open(fpath) as f:
                data = json.load(f)
        except json.JSONDecodeError as e:
            per_file_issues.append(f"{entry}: invalid JSON: {e}")
            continue

        if not isinstance(data, dict):
            per_file_issues.append(f"{entry}: not an object")
            continue

        actual = set(data.keys())
        all_keys.update(actual)

        extra = actual - EXPECTED_KEYS
        if extra:
            per_file_issues.append(f"{entry}: EXTRA keys: {extra}")

        missing = REQUIRED_KEYS - actual
        if missing:
            per_file_issues.append(f"{entry}: MISSING keys: {missing}")

    s.stat("Session files", file_count)
    s.stat("Bridge-auth key sidecars", key_file_count)
    s.stat("All unique keys collected", sorted(all_keys))

    extra_overall = all_keys - EXPECTED_KEYS
    missing_overall = REQUIRED_KEYS - all_keys
    if extra_overall:
        s.gap(f"EXTRA keys not in ActiveSessionFile type: {sorted(extra_overall)}")
    if missing_overall:
        s.gap(f"MISSING keys never seen in data: {sorted(missing_overall)}")

    if per_file_issues:
        for issue in per_file_issues[:15]:
            s.gap(issue)

    if not extra_overall and not missing_overall and not per_file_issues:
        s.info("All session files match ActiveSessionFile type perfectly")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 10. CACHE
# ═══════════════════════════════════════════════════════════════════════════════

def validate_cache() -> Section:
    s = Section("10. cache/  —  CacheDirectory type")
    cache_dir = CLAUDE_DIR / "cache"

    if not cache_dir.exists():
        s.gap("Directory does not exist")
        return s

    KNOWN_FILES = {"changelog.md", "my-closed-issues.json"}

    entries = sorted(os.listdir(cache_dir))
    s.stat("All files", entries)

    unexpected = set(entries) - KNOWN_FILES
    dirs = [e for e in entries if (cache_dir / e).is_dir()]
    if dirs:
        s.gap(f"Unexpected directories: {dirs}")
    if unexpected:
        s.gap(f"Unknown files: {sorted(unexpected)}")

    if not unexpected and not dirs:
        s.info("All cache files match CacheDirectory")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# 11. SUBAGENT META.JSON
# ═══════════════════════════════════════════════════════════════════════════════

def validate_subagent_meta() -> Section:
    s = Section("11. subagent meta.json  —  SubagentMeta type")
    projects_dir = CLAUDE_DIR / "projects"

    if not projects_dir.exists():
        s.gap("projects/ does not exist")
        return s

    EXPECTED_KEYS = interface_fields("SubagentMeta", "index.ts")

    meta_count = 0
    all_keys: set[str] = set()
    all_agent_types: set[str] = set()
    per_file_issues: list[str] = []

    # Walk through all project dirs -> session dirs -> subagents/
    for project_dir in sorted(projects_dir.iterdir()):
        if not project_dir.is_dir():
            continue
        for session_dir in sorted(project_dir.iterdir()):
            if not session_dir.is_dir():
                continue
            subagents_dir = session_dir / "subagents"
            if not subagents_dir.exists():
                continue
            for entry in sorted(os.listdir(subagents_dir)):
                if not entry.endswith(".meta.json"):
                    continue
                fpath = subagents_dir / entry
                meta_count += 1

                try:
                    with open(fpath) as f:
                        data = json.load(f)
                except json.JSONDecodeError as e:
                    per_file_issues.append(f"{entry}: invalid JSON: {e}")
                    continue

                if not isinstance(data, dict):
                    per_file_issues.append(f"{entry}: not an object")
                    continue

                actual = set(data.keys())
                all_keys.update(actual)

                extra = actual - EXPECTED_KEYS
                if extra:
                    per_file_issues.append(f"{entry}: EXTRA keys: {extra}")

                agent_type = data.get("agentType")
                if agent_type:
                    all_agent_types.add(agent_type)

    s.stat("Meta files found", meta_count)
    s.stat("All unique keys", sorted(all_keys))
    s.stat("All unique agentType values", sorted(all_agent_types))

    extra_overall = all_keys - EXPECTED_KEYS
    if extra_overall:
        s.gap(f"EXTRA keys not in SubagentMeta type: {sorted(extra_overall)}")

    if per_file_issues:
        for issue in per_file_issues[:15]:
            s.gap(issue)

    if not extra_overall and not per_file_issues:
        s.info("All meta.json files match SubagentMeta type perfectly")

    return s


# ═══════════════════════════════════════════════════════════════════════════════
# MAIN
# ═══════════════════════════════════════════════════════════════════════════════

def main() -> int:
    print("=" * 78)
    print("  @spaghetti/core  —  Secondary Data Validation")
    print(f"  Source: {CLAUDE_DIR}")
    print("=" * 78)

    sections = [
        validate_todos(),
        validate_tasks(),
        validate_file_history(),
        validate_debug(),
        validate_shell_snapshots(),
        validate_paste_cache(),
        validate_session_env(),
        validate_ide(),
        validate_sessions(),
        validate_cache(),
        validate_subagent_meta(),
    ]

    for section in sections:
        section.print_report()

    # Final summary
    passed = sum(1 for sec in sections if sec.passed)
    failed = sum(1 for sec in sections if not sec.passed)

    print("=" * 78)
    print(f"  FINAL RESULT: {passed} PASSED, {failed} FAILED out of {len(sections)} checks")
    if failed == 0:
        print("  All @spaghetti/core types match real data!")
    else:
        print("  Some types need updates — see [GAP] items above")
    print("=" * 78)
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
