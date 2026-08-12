#!/usr/bin/env python3
"""RFC 011 ownership ratchet.

The migration starts with intentional legacy exceptions. This check discovers
the current owners from source rather than trusting a hand-maintained count,
then rejects only additions to the committed allowlist. Removing an exception
is always safe and does not require editing the manifest immediately.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Callable


REPO_ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = Path(__file__).with_name("rfc011-legacy-boundaries.json")

SQL_AUTHORITY_RE = re.compile(r"\bSqliteService\b")
SQL_DRIVER_RE = re.compile(
    r"(?:\bfrom\s+|\brequire\s*\(\s*|\bimport\s*\(\s*)"
    r"['\"](?:node:sqlite|better-sqlite3)['\"]"
)
QUERY_MUTATION_RE = re.compile(r"\b(?:ensure|rebuild)\w*Projection\s*\(")
SOURCE_RUNTIME_RE = re.compile(
    r"(?:^|[-_/])(?:live|watch|watcher|checkpoint|writer|query|lifecycle-owner)(?:[-_/\.]|$)"
)
SOURCE_ID_LITERAL_RE = re.compile(r'"(?:claude-code|codex|grok)"')
SOURCE_LAYER_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|grok|engine|orchestrate|napi_engine)"
    r"|rusqlite|napi|sonic_rs|serde_json)(?:::|\b)"
)
ADAPTER_STORAGE_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:engine|orchestrate|napi_engine|core::(?:schema|writer|event))"
    r"|rusqlite|napi)(?:::|\b)"
)
MIGRATED_CLIENT_CONSUMERS = (
    "packages/sdk/src/observation-shadow.ts",
)
DIRECT_ENGINE_QUERY_RE = re.compile(
    r"\b(?:this\.)?engine\.(?:"
    r"health|overview|replayChanges|listHistoryProjects|listHistorySessions|getSession|getMessages|search|getTimeline|"
    r"listDelegations|listWorkflows|getWorkflow|listWorkflowMembers|listMemoryDocuments|listTaskCollections|listTasks|"
    r"listPlans|listToolResults|listArtifacts|listSources|getStats|getUsage|getUsageActivity|getRuntimeSnapshot|"
    r"getRunState|listTeams|getTeam|listTeamInboxes|listTeamInboxMessages"
    r")\s*\("
)


def repo_path(path: Path) -> str:
    return path.relative_to(REPO_ROOT).as_posix()


def production_typescript() -> list[Path]:
    root = REPO_ROOT / "packages/sdk/src"
    return sorted(
        path
        for path in root.rglob("*")
        if path.suffix in {".ts", ".tsx"}
        and "__tests__" not in path.parts
        and not path.name.endswith(".test.ts")
        and not path.name.endswith(".test.tsx")
    )


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def discover_typescript_sql_authorities() -> set[str]:
    return {
        repo_path(path)
        for path in production_typescript()
        if SQL_AUTHORITY_RE.search(read(path))
    }


def discover_typescript_sql_drivers() -> set[str]:
    return {
        repo_path(path)
        for path in production_typescript()
        if SQL_DRIVER_RE.search(read(path))
    }


def discover_query_projection_mutators() -> set[str]:
    return {
        repo_path(path)
        for path in production_typescript()
        if "query" in path.stem.lower() and QUERY_MUTATION_RE.search(read(path))
    }


def discover_source_runtime_services() -> set[str]:
    root = REPO_ROOT / "packages/sdk/src/sources"
    found: set[str] = set()
    for path in production_typescript():
        try:
            relative = path.relative_to(root)
        except ValueError:
            continue
        # A path directly under `sources/` is shared infrastructure. A path
        # below a source-id directory is adapter-local and must not grow its
        # own runtime control plane.
        if len(relative.parts) > 1 and SOURCE_RUNTIME_RE.search(relative.as_posix().lower()):
            found.add(repo_path(path))
    return found


def production_rust_text(path: Path) -> str:
    text = read(path)
    # The crate keeps conventional test modules at the end of each file.
    # Source ids in fixture assertions are not production dispatch.
    return text.split("\n#[cfg(test)]", maxsplit=1)[0]


def discover_rust_common_source_dispatch() -> set[str]:
    root = REPO_ROOT / "crates/spaghetti-napi/src"
    adapter_roots = {"claude", "codex", "grok"}
    found: set[str] = set()
    for path in sorted(root.rglob("*.rs")):
        relative = path.relative_to(root)
        if relative.parts[0] in adapter_roots:
            continue
        if SOURCE_ID_LITERAL_RE.search(production_rust_text(path)):
            found.add(repo_path(path))
    return found


def discover_rust_source_boundary_violations() -> set[str]:
    root = REPO_ROOT / "crates/spaghetti-napi/src/source"
    if not root.exists():
        return set()
    return {
        repo_path(path)
        for path in sorted(root.rglob("*.rs"))
        if SOURCE_LAYER_FORBIDDEN_RE.search(production_rust_text(path))
    }


def discover_rust_adapter_storage_boundary_violations() -> set[str]:
    root = REPO_ROOT / "crates/spaghetti-napi/src"
    paths = list((root / "adapter").rglob("*.rs"))
    paths.extend(
        path
        for adapter in ("claude", "codex", "grok")
        if (path := root / adapter / "adapter.rs").exists()
    )
    return {
        repo_path(path)
        for path in sorted(paths)
        if ADAPTER_STORAGE_FORBIDDEN_RE.search(production_rust_text(path))
    }


def discover_migrated_client_direct_engine_queries() -> set[str]:
    """Once a consumer moves to SpaghettiClient, direct N-API reads cannot return."""
    return {
        relative
        for relative in MIGRATED_CLIENT_CONSUMERS
        if DIRECT_ENGINE_QUERY_RE.search(read(REPO_ROOT / relative))
    }


DISCOVERERS: dict[str, Callable[[], set[str]]] = {
    "typescript_sql_authorities": discover_typescript_sql_authorities,
    "typescript_sql_drivers": discover_typescript_sql_drivers,
    "typescript_query_projection_mutators": discover_query_projection_mutators,
    "source_specific_runtime_services": discover_source_runtime_services,
    "rust_common_source_dispatch": discover_rust_common_source_dispatch,
    "rust_source_boundary_violations": discover_rust_source_boundary_violations,
    "rust_adapter_storage_boundary_violations": discover_rust_adapter_storage_boundary_violations,
    "migrated_client_direct_engine_queries": discover_migrated_client_direct_engine_queries,
}


def main() -> int:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    rules = manifest.get("rules", {})
    failures: list[tuple[str, list[str]]] = []

    if set(rules) != set(DISCOVERERS):
        missing = sorted(set(DISCOVERERS) - set(rules))
        unknown = sorted(set(rules) - set(DISCOVERERS))
        print(f"error: malformed RFC 011 manifest; missing={missing}, unknown={unknown}")
        return 1

    print("RFC 011 architecture ownership ratchet")
    for name, discover in DISCOVERERS.items():
        actual = discover()
        allowed = set(rules[name]["allowed"])
        additions = sorted(actual - allowed)
        retired = allowed - actual
        state = "FAIL" if additions else "ok"
        print(f"  {state:4}  {name}: {len(actual)} active, {len(retired)} retired")
        if additions:
            failures.append((name, additions))

    if failures:
        print("\nNew legacy ownership is forbidden by RFC 011:")
        for name, paths in failures:
            print(f"\n{name}:")
            for path in paths:
                print(f"  + {path}")
        print(
            "\nMove the responsibility behind the common Rust engine boundary. "
            "Only update the allowlist when the RFC itself intentionally changes."
        )
        return 1

    print("RFC 011 boundaries pass (the allowlists may only shrink).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
