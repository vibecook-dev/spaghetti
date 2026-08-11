#!/usr/bin/env python3
"""
Validate @spaghetti/core config and secondary types against REAL data in ~/.claude.

Checks:
  1. settings.json / settings.local.json — keys vs SettingsFile
  2. stats-cache.json — keys vs StatsCacheFile, model names
  3. history.jsonl — keys vs HistoryEntry
  4. plugins/ — installed_plugins, known_marketplaces, blocklist, install-counts-cache
  5. telemetry/ — event_name values vs TelemetryEventName union
  6. statsig/ — cached evaluations structure
  7. teams/ — config.json vs TeamConfig, inbox messages vs InboxMessage
  8. backups/ — backup file keys vs ClaudeGlobalState
  9. top-level files — report unmodeled files
"""

import json
import os
import re
import sys
from pathlib import Path

from ts_types import (
    discriminated_variants,
    interface_fields,
    interface_optional_fields,
    union_members,
)

CLAUDE_DIR = Path.home() / ".claude"

# ═══════════════════════════════════════════════════════════════════════════════
# Known type definitions (from @spaghetti/core types)
# ═══════════════════════════════════════════════════════════════════════════════

# SettingsFile fields, read from the interface rather than copied. The copy
# this replaced stopped at `hooks`, so five keys already present in TS
# (tui, autoCompactEnabled, agentPushNotifEnabled, skipWorkflowUsageWarning,
# skipAutoPermissionPrompt) were reported as unmodelled.
SETTINGS_FIELDS_REQUIRED = set()  # permissions was required in type but may be absent in local
SETTINGS_ALL_FIELDS = interface_fields("SettingsFile", "claude/toplevel-files-data.ts")
SETTINGS_FIELDS_OPTIONAL = SETTINGS_ALL_FIELDS - SETTINGS_FIELDS_REQUIRED

# PermissionsConfig fields
PERMISSIONS_FIELDS_REQUIRED = {"allow"}
PERMISSIONS_FIELDS_OPTIONAL = {"deny"}
PERMISSIONS_ALL_FIELDS = PERMISSIONS_FIELDS_REQUIRED | PERMISSIONS_FIELDS_OPTIONAL

# StatsCacheFile fields, read from the interface for the same reason
# SettingsFile is: a hand-copied set silently rots when Claude Code adds or
# drops a key. Optional (`?:`) members are "may be absent", which is how a
# dropped field like totalSpeculationTimeSavedMs stays modelled for older
# caches without failing against current ones.
STATS_CACHE_ALL_FIELDS = interface_fields("StatsCacheFile", "claude/toplevel-files-data.ts")
STATS_CACHE_FIELDS_OPTIONAL = interface_optional_fields("StatsCacheFile", "claude/toplevel-files-data.ts")
STATS_CACHE_FIELDS_REQUIRED = STATS_CACHE_ALL_FIELDS - STATS_CACHE_FIELDS_OPTIONAL

# DailyActivity fields
DAILY_ACTIVITY_FIELDS = {"date", "messageCount", "sessionCount", "toolCallCount"}

# DailyModelTokens fields
DAILY_MODEL_TOKENS_FIELDS = {"date", "tokensByModel"}

# ModelUsageStats fields
MODEL_USAGE_STATS_FIELDS = {
    "inputTokens", "outputTokens", "cacheReadInputTokens",
    "cacheCreationInputTokens", "webSearchRequests", "costUSD",
    "contextWindow", "maxOutputTokens",
}

# LongestSession fields
LONGEST_SESSION_FIELDS = {"sessionId", "duration", "messageCount", "timestamp"}

# HistoryEntry fields
HISTORY_ENTRY_FIELDS = {"display", "pastedContents", "timestamp", "project", "sessionId"}

# InstalledPluginsFile top-level
INSTALLED_PLUGINS_TOP = {"version", "plugins"}

# InstalledPluginEntry fields
INSTALLED_PLUGIN_ENTRY_REQUIRED = {"scope", "installPath", "version", "installedAt", "lastUpdated"}
INSTALLED_PLUGIN_ENTRY_OPTIONAL = {"gitCommitSha"}

# KnownMarketplaceEntry fields
KNOWN_MARKETPLACE_ENTRY_REQUIRED = {"source", "installLocation", "lastUpdated"}
KNOWN_MARKETPLACE_ENTRY_OPTIONAL = {"autoUpdate"}

# MarketplaceSource fields
MARKETPLACE_SOURCE_COMMON_FIELDS = {"source"}
MARKETPLACE_SOURCE_FIELDS_BY_KIND = discriminated_variants(
    "MarketplaceSource", "source", "claude/plugins-data.ts"
)

# InstallCountsCacheFile fields
INSTALL_COUNTS_CACHE_TOP = {"version", "fetchedAt", "counts"}

# PluginInstallCount fields
PLUGIN_INSTALL_COUNT_FIELDS = {"plugin", "unique_installs"}

# BlocklistFile fields (inferred from actual data — fetchedAt + plugins array)
BLOCKLIST_TOP = {"fetchedAt", "plugins"}
BLOCKLIST_ENTRY_FIELDS = {"plugin", "added_at", "reason", "text"}

TELEMETRY_TS = "claude/telemetry-data.ts"

KNOWN_TELEMETRY_EVENT_NAMES = union_members("TelemetryEventName", TELEMETRY_TS)

TELEMETRY_EVENT_DATA_KEYS = interface_fields("TelemetryEventData", TELEMETRY_TS)

TELEMETRY_ENV_KEYS = {
    "platform", "node_version", "terminal", "package_managers",
    "runtimes", "is_running_with_bun", "is_ci", "is_claubbit",
    "is_github_action", "is_claude_code_action", "is_claude_ai_auth",
    "version", "arch", "is_claude_code_remote", "deployment_environment",
    "is_conductor", "version_base",
}

# StatsigCachedEvaluations outer keys
STATSIG_CACHED_EVAL_OUTER = {"source", "data", "receivedAt", "stableID", "fullUserHash"}

# StatsigEvaluationsData inner keys
STATSIG_EVAL_DATA_INNER = {
    "feature_gates", "dynamic_configs", "layer_configs",
    "sdkParams", "has_updates", "generator", "time",
    "company_lcut", "evaluated_keys", "hash_used",
    "derived_fields", "hashed_sdk_key_used",
    "can_record_session", "recording_blocked",
    "session_recording_rate", "auto_capture_settings",
    "target_app_used", "full_checksum",
}

# StatsigSessionId fields
STATSIG_SESSION_ID_FIELDS = {"sessionID", "startTime", "lastUpdate"}

# StatsigFailedLogEvent fields
STATSIG_FAILED_LOG_EVENT = {"eventName", "metadata", "user", "time"}
STATSIG_USER_FIELDS = {"customIDs", "userID", "appVersion", "custom", "statsigEnvironment"}

# TeamConfig fields
TEAM_CONFIG_REQUIRED = {"name", "createdAt", "leadAgentId", "leadSessionId", "members"}
TEAM_CONFIG_OPTIONAL = {"description"}

# TeamMember fields
TEAM_MEMBER_REQUIRED = {"agentId", "name", "joinedAt", "tmuxPaneId", "cwd", "subscriptions"}
TEAM_MEMBER_OPTIONAL = {"model", "prompt", "color", "planModeRequired", "backendType", "agentType"}

# InboxMessage fields
INBOX_MESSAGE_REQUIRED = {"from", "text", "timestamp", "read"}
INBOX_MESSAGE_OPTIONAL = {"summary", "color", "msg_id", "msgV", "type"}

# ClaudeGlobalState known fields
CLAUDE_GLOBAL_STATE_KNOWN = {"numStartups", "installMethod", "autoUpdates",
                              "hasSeenTasksHint", "tipsHistory"}

# ActiveSessionFile fields
ACTIVE_SESSION_FIELDS_REQUIRED = {"pid", "sessionId", "cwd", "startedAt"}
ACTIVE_SESSION_FIELDS_OPTIONAL = (
    interface_fields("ActiveSessionFile", "claude/toplevel-files-data.ts")
    - ACTIVE_SESSION_FIELDS_REQUIRED
)

# McpNeedsAuthCache — Record<string, {timestamp: number}>
# (validated structurally)

# Known top-level files in ~/.claude/
KNOWN_TOP_LEVEL_FILES = {
    "settings.json",
    "settings.local.json",
    "stats-cache.json",
    "history.jsonl",
    "statusline-command.sh",
    "mcp-needs-auth-cache.json",
}

# Known top-level directories
KNOWN_TOP_LEVEL_DIRS = {
    "backups", "cache", "debug", "file-history", "ide",
    "paste-cache", "plans", "plugins", "projects", "session-env",
    "sessions", "shell-snapshots", "statsig", "tasks", "teams",
    "telemetry", "todos",
}

# ═══════════════════════════════════════════════════════════════════════════════
# Helpers
# ═══════════════════════════════════════════════════════════════════════════════

passed = 0
failed = 0
warnings = 0


def ok(msg):
    global passed
    passed += 1
    print(f"  PASS: {msg}")


def fail(msg):
    global failed
    failed += 1
    print(f"  FAIL: {msg}")


def warn(msg):
    global warnings
    warnings += 1
    print(f"  WARN: {msg}")


def check_keys(label, actual_keys, expected, optional=None):
    """Check actual keys against expected required + optional sets."""
    if optional is None:
        optional = set()
    all_known = expected | optional
    actual = set(actual_keys)
    missing = expected - actual
    extra = actual - all_known
    if missing:
        fail(f"{label}: MISSING keys: {sorted(missing)}")
    if extra:
        fail(f"{label}: EXTRA keys: {sorted(extra)}")
    if not missing and not extra:
        ok(f"{label}: keys match ({len(actual)} found)")
    return missing, extra


def load_json(path):
    """Load a JSON file, return (data, error_msg)."""
    try:
        with open(path) as f:
            return json.load(f), None
    except json.JSONDecodeError as e:
        return None, f"invalid JSON: {e}"
    except IOError as e:
        return None, f"read error: {e}"


# ═══════════════════════════════════════════════════════════════════════════════
# Section 1: settings.json
# ═══════════════════════════════════════════════════════════════════════════════

def validate_settings():
    print("\n" + "=" * 78)
    print("1. SETTINGS FILES (settings.json, settings.local.json)")
    print("=" * 78)

    for fname in ("settings.json", "settings.local.json"):
        path = CLAUDE_DIR / fname
        print(f"\n  --- {fname} ---")
        if not path.exists():
            warn(f"{fname}: not found (may be expected)")
            continue

        data, err = load_json(path)
        if err:
            fail(f"{fname}: {err}")
            continue

        # Top-level keys
        check_keys(f"{fname} top-level", data.keys(),
                    SETTINGS_FIELDS_REQUIRED, SETTINGS_FIELDS_OPTIONAL)

        # Permissions sub-object
        if "permissions" in data and isinstance(data["permissions"], dict):
            check_keys(f"{fname} permissions", data["permissions"].keys(),
                        PERMISSIONS_FIELDS_REQUIRED, PERMISSIONS_FIELDS_OPTIONAL)

        # StatusLine sub-object
        if "statusLine" in data and isinstance(data["statusLine"], dict):
            sl = data["statusLine"]
            sl_known = {"type", "command"}
            sl_extra = set(sl.keys()) - sl_known
            if sl_extra:
                fail(f"{fname} statusLine: EXTRA keys: {sorted(sl_extra)}")
            else:
                ok(f"{fname} statusLine: keys match")


# ═══════════════════════════════════════════════════════════════════════════════
# Section 2: stats-cache.json
# ═══════════════════════════════════════════════════════════════════════════════

def validate_stats_cache():
    print("\n" + "=" * 78)
    print("2. STATS-CACHE.JSON")
    print("=" * 78)

    path = CLAUDE_DIR / "stats-cache.json"
    if not path.exists():
        warn("stats-cache.json: not found")
        return

    data, err = load_json(path)
    if err:
        fail(f"stats-cache.json: {err}")
        return

    # Top-level keys
    check_keys(
        "stats-cache.json top-level",
        data.keys(),
        STATS_CACHE_FIELDS_REQUIRED,
        STATS_CACHE_FIELDS_OPTIONAL,
    )

    # DailyActivity entries
    daily = data.get("dailyActivity", [])
    if isinstance(daily, list) and len(daily) > 0:
        sample = daily[0]
        check_keys("stats-cache.json dailyActivity[0]", sample.keys(), DAILY_ACTIVITY_FIELDS)
    else:
        warn("stats-cache.json: no dailyActivity entries to check")

    # DailyModelTokens entries
    dmt = data.get("dailyModelTokens", [])
    if isinstance(dmt, list) and len(dmt) > 0:
        sample = dmt[0]
        check_keys("stats-cache.json dailyModelTokens[0]", sample.keys(), DAILY_MODEL_TOKENS_FIELDS)
    else:
        warn("stats-cache.json: no dailyModelTokens entries to check")

    # ModelUsage — list all model names
    mu = data.get("modelUsage", {})
    if isinstance(mu, dict):
        models = sorted(mu.keys())
        print(f"\n  Models in modelUsage ({len(models)}):")
        for m in models:
            print(f"    - {m}")

        # Check ModelUsageStats fields for each model
        for model_name, stats in mu.items():
            if isinstance(stats, dict):
                check_keys(f"modelUsage[{model_name}]", stats.keys(), MODEL_USAGE_STATS_FIELDS)

    # LongestSession
    ls = data.get("longestSession", {})
    if isinstance(ls, dict):
        check_keys("longestSession", ls.keys(), LONGEST_SESSION_FIELDS)


# ═══════════════════════════════════════════════════════════════════════════════
# Section 3: history.jsonl
# ═══════════════════════════════════════════════════════════════════════════════

def validate_history():
    print("\n" + "=" * 78)
    print("3. HISTORY.JSONL")
    print("=" * 78)

    path = CLAUDE_DIR / "history.jsonl"
    if not path.exists():
        warn("history.jsonl: not found")
        return

    all_keys = set()
    entry_count = 0
    errors = 0

    try:
        with open(path, "r", errors="replace") as f:
            for line_num, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                    entry_count += 1
                    if isinstance(obj, dict):
                        all_keys.update(obj.keys())
                except json.JSONDecodeError:
                    errors += 1
                    if errors <= 3:
                        fail(f"history.jsonl:{line_num}: invalid JSON")
    except IOError as e:
        fail(f"history.jsonl: read error: {e}")
        return

    print(f"  Entries parsed: {entry_count}")
    if errors > 3:
        fail(f"history.jsonl: {errors} JSON parse errors total")

    check_keys("history.jsonl unique keys", all_keys, HISTORY_ENTRY_FIELDS)


# ═══════════════════════════════════════════════════════════════════════════════
# Section 4: plugins/
# ═══════════════════════════════════════════════════════════════════════════════

def validate_plugins():
    print("\n" + "=" * 78)
    print("4. PLUGINS DIRECTORY")
    print("=" * 78)

    plugins_dir = CLAUDE_DIR / "plugins"
    if not plugins_dir.exists():
        warn("plugins/: not found")
        return

    # --- installed_plugins.json ---
    print("\n  --- installed_plugins.json ---")
    ip_path = plugins_dir / "installed_plugins.json"
    if ip_path.exists():
        data, err = load_json(ip_path)
        if err:
            fail(f"installed_plugins.json: {err}")
        elif isinstance(data, dict):
            check_keys("installed_plugins.json top-level", data.keys(), INSTALLED_PLUGINS_TOP)

            plugins = data.get("plugins", {})
            if isinstance(plugins, dict):
                for key, entries in plugins.items():
                    if not isinstance(entries, list):
                        fail(f"installed_plugins.json: plugins['{key}'] is not an array")
                        continue
                    for i, entry in enumerate(entries):
                        if isinstance(entry, dict):
                            check_keys(
                                f"installed_plugins['{key}'][{i}]",
                                entry.keys(),
                                INSTALLED_PLUGIN_ENTRY_REQUIRED,
                                INSTALLED_PLUGIN_ENTRY_OPTIONAL,
                            )
    else:
        warn("installed_plugins.json: not found")

    # --- known_marketplaces.json ---
    print("\n  --- known_marketplaces.json ---")
    km_path = plugins_dir / "known_marketplaces.json"
    if km_path.exists():
        data, err = load_json(km_path)
        if err:
            fail(f"known_marketplaces.json: {err}")
        elif isinstance(data, dict):
            for name, entry in data.items():
                if isinstance(entry, dict):
                    check_keys(
                        f"known_marketplaces['{name}']",
                        entry.keys(),
                        KNOWN_MARKETPLACE_ENTRY_REQUIRED,
                        KNOWN_MARKETPLACE_ENTRY_OPTIONAL,
                    )
                    src = entry.get("source", {})
                    if isinstance(src, dict):
                        source_kind = src.get("source")
                        specific_fields = MARKETPLACE_SOURCE_FIELDS_BY_KIND.get(source_kind, set())
                        check_keys(
                            f"known_marketplaces['{name}'].source",
                            src.keys(),
                            MARKETPLACE_SOURCE_COMMON_FIELDS | specific_fields,
                        )
    else:
        warn("known_marketplaces.json: not found")

    # --- blocklist.json ---
    print("\n  --- blocklist.json ---")
    bl_path = plugins_dir / "blocklist.json"
    if bl_path.exists():
        data, err = load_json(bl_path)
        if err:
            fail(f"blocklist.json: {err}")
        elif isinstance(data, dict):
            check_keys("blocklist.json top-level", data.keys(), BLOCKLIST_TOP)

            bl_plugins = data.get("plugins", [])
            if isinstance(bl_plugins, list):
                for i, entry in enumerate(bl_plugins[:5]):
                    if isinstance(entry, dict):
                        check_keys(f"blocklist.plugins[{i}]", entry.keys(), BLOCKLIST_ENTRY_FIELDS)
    else:
        warn("blocklist.json: not found")

    # --- install-counts-cache.json ---
    print("\n  --- install-counts-cache.json ---")
    icc_path = plugins_dir / "install-counts-cache.json"
    if icc_path.exists():
        data, err = load_json(icc_path)
        if err:
            fail(f"install-counts-cache.json: {err}")
        elif isinstance(data, dict):
            check_keys("install-counts-cache.json top-level", data.keys(), INSTALL_COUNTS_CACHE_TOP)

            counts = data.get("counts", [])
            if isinstance(counts, list):
                for i, c in enumerate(counts[:5]):
                    if isinstance(c, dict):
                        check_keys(f"install-counts-cache.counts[{i}]", c.keys(), PLUGIN_INSTALL_COUNT_FIELDS)
    else:
        warn("install-counts-cache.json: not found")


# ═══════════════════════════════════════════════════════════════════════════════
# Section 5: telemetry/
# ═══════════════════════════════════════════════════════════════════════════════

def validate_telemetry():
    print("\n" + "=" * 78)
    print("5. TELEMETRY DIRECTORY")
    print("=" * 78)

    telemetry_dir = CLAUDE_DIR / "telemetry"
    if not telemetry_dir.exists():
        warn("telemetry/: not found")
        return

    file_count = 0
    line_count = 0
    unknown_event_names = set()
    unknown_event_data_keys = set()
    unknown_env_keys = set()
    all_event_names = set()
    max_files_to_check = 20  # sample to avoid slowness

    filename_re = re.compile(r'^1p_failed_events\.([0-9a-f-]+)\.([0-9a-f-]+)\.json$')

    for entry in sorted(os.listdir(telemetry_dir)):
        fpath = telemetry_dir / entry
        if fpath.is_dir():
            warn(f"telemetry/: unexpected directory: {entry}")
            continue

        file_count += 1
        if file_count > max_files_to_check:
            continue

        m = filename_re.match(entry)
        if not m:
            warn(f"telemetry/: filename doesn't match pattern: {entry}")

        size = os.path.getsize(fpath)
        if size == 0:
            continue

        try:
            with open(fpath, "r", errors="replace") as fp:
                for line_num, line in enumerate(fp, 1):
                    line = line.strip()
                    if not line:
                        continue
                    line_count += 1
                    try:
                        obj = json.loads(line)
                    except json.JSONDecodeError:
                        continue

                    if not isinstance(obj, dict):
                        continue

                    # Check event_data
                    ed = obj.get("event_data", {})
                    if isinstance(ed, dict):
                        ed_unknown = set(ed.keys()) - TELEMETRY_EVENT_DATA_KEYS
                        unknown_event_data_keys.update(ed_unknown)

                        en = ed.get("event_name")
                        if en:
                            all_event_names.add(en)
                            if en not in KNOWN_TELEMETRY_EVENT_NAMES:
                                unknown_event_names.add(en)

                        env = ed.get("env", {})
                        if isinstance(env, dict):
                            env_unknown = set(env.keys()) - TELEMETRY_ENV_KEYS
                            unknown_env_keys.update(env_unknown)
        except IOError:
            pass

    total_files = len(list(telemetry_dir.iterdir()))
    print(f"  Total telemetry files: {total_files}")
    print(f"  Sampled files: {min(file_count, max_files_to_check)}")
    print(f"  Event lines parsed: {line_count}")

    print(f"\n  All event_name values found ({len(all_event_names)}):")
    for en in sorted(all_event_names):
        marker = " [NEW]" if en in unknown_event_names else ""
        print(f"    - {en}{marker}")

    if unknown_event_names:
        fail(f"telemetry: NEW event names not in TelemetryEventName: {sorted(unknown_event_names)}")
    else:
        ok("telemetry: all event_name values covered by TelemetryEventName")

    if unknown_event_data_keys:
        fail(f"telemetry: EXTRA event_data keys: {sorted(unknown_event_data_keys)}")
    else:
        ok("telemetry: all event_data keys match TelemetryEventData")

    if unknown_env_keys:
        fail(f"telemetry: EXTRA env keys: {sorted(unknown_env_keys)}")
    else:
        ok("telemetry: all env keys match TelemetryEnv")


# ═══════════════════════════════════════════════════════════════════════════════
# Section 6: statsig/
# ═══════════════════════════════════════════════════════════════════════════════

def validate_statsig():
    print("\n" + "=" * 78)
    print("6. STATSIG DIRECTORY")
    print("=" * 78)

    statsig_dir = CLAUDE_DIR / "statsig"
    if not statsig_dir.exists():
        warn("statsig/: not found")
        return

    cached_eval_re = re.compile(r'^statsig\.cached\.evaluations\.\w+$')
    failed_logs_re = re.compile(r'^statsig\.failed_logs\.\w+$')
    last_mod_re = re.compile(r'^statsig\.last_modified_time\.evaluations$')
    session_id_re = re.compile(r'^statsig\.session_id\.\w+$')
    stable_id_re = re.compile(r'^statsig\.stable_id\.\w+$')

    file_count = 0
    for entry in sorted(os.listdir(statsig_dir)):
        fpath = statsig_dir / entry
        if fpath.is_dir():
            warn(f"statsig/: unexpected directory: {entry}")
            continue

        file_count += 1
        size = os.path.getsize(fpath)

        if cached_eval_re.match(entry):
            if size == 0:
                continue
            data, err = load_json(fpath)
            if err:
                fail(f"statsig/{entry}: {err}")
                continue
            check_keys(f"statsig/{entry} outer", data.keys(), STATSIG_CACHED_EVAL_OUTER)

            inner_str = data.get("data", "")
            if inner_str:
                try:
                    inner = json.loads(inner_str)
                    check_keys(f"statsig/{entry} inner data", inner.keys(), STATSIG_EVAL_DATA_INNER)
                except json.JSONDecodeError as e:
                    fail(f"statsig/{entry}: inner data parse error: {e}")

        elif failed_logs_re.match(entry):
            if size == 0:
                continue
            data, err = load_json(fpath)
            if err:
                fail(f"statsig/{entry}: {err}")
                continue
            if isinstance(data, list):
                for i, evt in enumerate(data[:5]):
                    if isinstance(evt, dict):
                        check_keys(f"statsig/{entry}[{i}]", evt.keys(), STATSIG_FAILED_LOG_EVENT)
                        user = evt.get("user", {})
                        if isinstance(user, dict):
                            check_keys(f"statsig/{entry}[{i}].user", user.keys(), STATSIG_USER_FIELDS)
                ok(f"statsig/{entry}: array of {len(data)} events")
            else:
                fail(f"statsig/{entry}: not an array")

        elif last_mod_re.match(entry):
            if size == 0:
                continue
            data, err = load_json(fpath)
            if err:
                fail(f"statsig/{entry}: {err}")
                continue
            if isinstance(data, dict):
                all_numeric = all(isinstance(v, (int, float)) for v in data.values())
                if all_numeric:
                    ok(f"statsig/{entry}: Record<string, number> ({len(data)} entries)")
                else:
                    fail(f"statsig/{entry}: not all values are numbers")
            else:
                fail(f"statsig/{entry}: not an object")

        elif session_id_re.match(entry):
            if size == 0:
                continue
            data, err = load_json(fpath)
            if err:
                fail(f"statsig/{entry}: {err}")
                continue
            check_keys(f"statsig/{entry}", data.keys(), STATSIG_SESSION_ID_FIELDS)

        elif stable_id_re.match(entry):
            if size == 0:
                continue
            data, err = load_json(fpath)
            if err:
                fail(f"statsig/{entry}: {err}")
                continue
            if isinstance(data, str):
                ok(f"statsig/{entry}: string value")
            else:
                fail(f"statsig/{entry}: expected string, got {type(data).__name__}")

        else:
            fail(f"statsig/: UNKNOWN file pattern: {entry}")

    print(f"\n  Statsig files: {file_count}")


# ═══════════════════════════════════════════════════════════════════════════════
# Section 7: teams/
# ═══════════════════════════════════════════════════════════════════════════════

def validate_teams():
    print("\n" + "=" * 78)
    print("7. TEAMS DIRECTORY")
    print("=" * 78)

    teams_dir = CLAUDE_DIR / "teams"
    if not teams_dir.exists():
        warn("teams/: not found")
        return

    team_count = 0
    for team_name in sorted(os.listdir(teams_dir)):
        team_path = teams_dir / team_name
        if not team_path.is_dir():
            warn(f"teams/: unexpected file: {team_name}")
            continue

        team_count += 1
        print(f"\n  --- team: {team_name} ---")

        # config.json
        config_path = team_path / "config.json"
        if config_path.exists():
            data, err = load_json(config_path)
            if err:
                fail(f"teams/{team_name}/config.json: {err}")
            elif isinstance(data, dict):
                check_keys(
                    f"teams/{team_name}/config.json",
                    data.keys(),
                    TEAM_CONFIG_REQUIRED,
                    TEAM_CONFIG_OPTIONAL,
                )

                # Check members
                members = data.get("members", [])
                if isinstance(members, list):
                    for i, member in enumerate(members):
                        if isinstance(member, dict):
                            check_keys(
                                f"teams/{team_name}/members[{i}] ({member.get('name', '?')})",
                                member.keys(),
                                TEAM_MEMBER_REQUIRED,
                                TEAM_MEMBER_OPTIONAL,
                            )
        else:
            warn(f"teams/{team_name}/config.json: not found")

        # inboxes/
        inboxes_dir = team_path / "inboxes"
        if inboxes_dir.exists():
            inbox_all_keys = set()
            inbox_file_count = 0
            for inbox_file in sorted(os.listdir(inboxes_dir)):
                inbox_path = inboxes_dir / inbox_file
                if not inbox_path.is_file() or not inbox_file.endswith(".json"):
                    continue
                inbox_file_count += 1
                data, err = load_json(inbox_path)
                if err:
                    fail(f"teams/{team_name}/inboxes/{inbox_file}: {err}")
                    continue
                if isinstance(data, list):
                    for msg in data:
                        if isinstance(msg, dict):
                            inbox_all_keys.update(msg.keys())

            if inbox_all_keys:
                check_keys(
                    f"teams/{team_name}/inboxes (all msg keys)",
                    inbox_all_keys,
                    INBOX_MESSAGE_REQUIRED,
                    INBOX_MESSAGE_OPTIONAL,
                )
            ok(f"teams/{team_name}/inboxes: {inbox_file_count} files checked")
        else:
            warn(f"teams/{team_name}/inboxes: not found")

    print(f"\n  Total teams: {team_count}")


# ═══════════════════════════════════════════════════════════════════════════════
# Section 8: backups/
# ═══════════════════════════════════════════════════════════════════════════════

def validate_backups():
    print("\n" + "=" * 78)
    print("8. BACKUPS DIRECTORY")
    print("=" * 78)

    backups_dir = CLAUDE_DIR / "backups"
    if not backups_dir.exists():
        warn("backups/: not found")
        return

    backup_files = sorted([
        f for f in os.listdir(backups_dir)
        if (backups_dir / f).is_file()
    ])

    if not backup_files:
        warn("backups/: no backup files found")
        return

    print(f"  Total backup files: {len(backup_files)}")

    # Sample the latest backup
    latest = backup_files[-1]
    path = backups_dir / latest
    data, err = load_json(path)
    if err:
        fail(f"backups/{latest}: {err}")
        return

    if not isinstance(data, dict):
        fail(f"backups/{latest}: not an object")
        return

    actual_keys = set(data.keys())
    known_keys = CLAUDE_GLOBAL_STATE_KNOWN
    extra = actual_keys - known_keys
    covered = actual_keys & known_keys

    print(f"  Sample: {latest}")
    print(f"  Total keys: {len(actual_keys)}")
    print(f"  Known keys: {sorted(covered)}")
    if extra:
        print(f"  Extra keys (covered by [key: string]: unknown): {len(extra)}")
        # Show first few
        for k in sorted(extra)[:15]:
            print(f"    - {k}")
        if len(extra) > 15:
            print(f"    ... and {len(extra) - 15} more")
        ok(f"backups/{latest}: {len(extra)} extra keys (OK per index signature)")
    else:
        ok(f"backups/{latest}: all keys match known fields")


# ═══════════════════════════════════════════════════════════════════════════════
# Section 9: Top-level files
# ═══════════════════════════════════════════════════════════════════════════════

def validate_top_level():
    print("\n" + "=" * 78)
    print("9. TOP-LEVEL FILES IN ~/.claude/")
    print("=" * 78)

    all_entries = sorted(os.listdir(CLAUDE_DIR))
    files = []
    dirs = []

    for entry in all_entries:
        full = CLAUDE_DIR / entry
        if full.is_dir():
            dirs.append(entry)
        elif full.is_file() or full.is_symlink():
            files.append(entry)

    # Check files
    unmodeled_files = [f for f in files if f not in KNOWN_TOP_LEVEL_FILES]
    modeled_files = [f for f in files if f in KNOWN_TOP_LEVEL_FILES]

    print(f"\n  Known files ({len(modeled_files)}):")
    for f in sorted(modeled_files):
        print(f"    - {f}")

    if unmodeled_files:
        print(f"\n  Unmodeled files ({len(unmodeled_files)}):")
        for f in sorted(unmodeled_files):
            full = CLAUDE_DIR / f
            size = os.path.getsize(full) if full.exists() else 0
            print(f"    - {f}  ({size} bytes)")
        warn(f"top-level: {len(unmodeled_files)} unmodeled file(s) found")
    else:
        ok("top-level: all files accounted for")

    # Check directories
    unmodeled_dirs = [d for d in dirs if d not in KNOWN_TOP_LEVEL_DIRS]
    if unmodeled_dirs:
        print(f"\n  Unmodeled directories ({len(unmodeled_dirs)}):")
        for d in sorted(unmodeled_dirs):
            print(f"    - {d}/")
        warn(f"top-level: {len(unmodeled_dirs)} unmodeled dir(s) found")
    else:
        ok("top-level: all directories accounted for")

    # Validate sessions/ (ActiveSessionFile)
    sessions_dir = CLAUDE_DIR / "sessions"
    if sessions_dir.exists():
        print(f"\n  --- sessions/ (ActiveSessionFile) ---")
        session_files = [f for f in os.listdir(sessions_dir) if f.endswith(".json")]
        if session_files:
            sample = session_files[0]
            data, err = load_json(sessions_dir / sample)
            if err:
                fail(f"sessions/{sample}: {err}")
            elif isinstance(data, dict):
                check_keys(
                    f"sessions/{sample}",
                    data.keys(),
                    ACTIVE_SESSION_FIELDS_REQUIRED,
                    ACTIVE_SESSION_FIELDS_OPTIONAL,
                )
            print(f"  Active session files: {len(session_files)}")

    # Validate mcp-needs-auth-cache.json
    mcp_path = CLAUDE_DIR / "mcp-needs-auth-cache.json"
    if mcp_path.exists():
        print(f"\n  --- mcp-needs-auth-cache.json ---")
        data, err = load_json(mcp_path)
        if err:
            fail(f"mcp-needs-auth-cache.json: {err}")
        elif isinstance(data, dict):
            all_valid = True
            for key, val in data.items():
                if not isinstance(val, dict) or "timestamp" not in val:
                    fail(f"mcp-needs-auth-cache['{key}']: expected {{timestamp: number}}")
                    all_valid = False
            if all_valid:
                ok(f"mcp-needs-auth-cache.json: {len(data)} entries, structure matches McpNeedsAuthCache")


# ═══════════════════════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════════════════════

def main():
    if not CLAUDE_DIR.exists():
        # Importing this module already exercised every interface_fields() /
        # union_members() lookup, so a renamed or deleted type has failed by
        # now. What is left needs real Claude Code output, which a CI runner
        # does not have — report that honestly instead of passing or failing.
        print(f"SKIPPED: {CLAUDE_DIR} does not exist (no real Claude Code data to validate against)")
        sys.exit(78)

    print(f"Validating @spaghetti/core types against {CLAUDE_DIR}")
    print(f"{'=' * 78}")

    validate_settings()
    validate_stats_cache()
    validate_history()
    validate_plugins()
    validate_telemetry()
    validate_statsig()
    validate_teams()
    validate_backups()
    validate_top_level()

    # ── Final Summary ─────────────────────────────────────────────────────────

    print("\n" + "=" * 78)
    print("VALIDATION SUMMARY")
    print("=" * 78)
    print(f"  PASSED:   {passed}")
    print(f"  FAILED:   {failed}")
    print(f"  WARNINGS: {warnings}")
    print()

    if failed == 0:
        print("RESULT: ALL CLEAR -- types match real data")
    else:
        print(f"RESULT: {failed} FAILURE(S) -- types need updating")

    print("=" * 78)
    return 1 if failed > 0 else 0


if __name__ == "__main__":
    sys.exit(main())
