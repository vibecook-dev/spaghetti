#!/usr/bin/env python3
"""RFC 011 production ownership boundary.

The checker follows the shipped TypeScript and default Rust runtime graphs.
Repository-only differential oracles may remain in-tree, but they must be
unreachable from package exports and absent from default native builds.
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
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
SOURCE_ID_CONCAT_RE = re.compile(
    r"\bconcat!\s*\([^)]*\b(?:claude|codex|grok)\b[^)]*\)"
)
CONCRETE_ADAPTER_DEPENDENCY_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|grok)(?:::|\b)"
    r"|(?:ClaudeCode|Codex|Grok)Adapter\b)"
)
SOURCE_LAYER_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|grok|engine|orchestrate|napi_engine)"
    r"|napi|sonic_rs|serde_json)(?:::|\b)"
)
SOURCE_LAYER_SQLITE_RE = re.compile(r"\brusqlite(?:::|\b)")
APPROVED_SOURCE_SQLITE_DRIVER = "crates/spaghetti-napi/src/source/sqlite_snapshot.rs"
ADAPTER_STORAGE_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:engine|orchestrate|napi_engine|core::(?:schema|writer|event))"
    r"|rusqlite|napi)(?:::|\b)"
)
RFC012_ADAPTER_ACCESS_AUTHORITY_RE = re.compile(
    r"\b(?:AccessBudget|AccessReservation|AccessReservationRequest|AccessObjectToken)\b"
)
RFC012_SEMANTIC_FORBIDDEN_RE = re.compile(
    r"(?:\bcrate::|\bsuper::(?:contract|facts|registry)(?:::|\b)"
    r"|\brusqlite(?:::|\b)|\bnapi(?:::|\b))"
)
RFC012_SUPPORT_FORBIDDEN_RE = re.compile(
    r"(?:\bcrate::|\bsuper::(?:::|\b)|\brusqlite(?:::|\b)|\bnapi(?:::|\b))"
)
BUILTIN_ADAPTER_PATHS = (
    "crates/spaghetti-napi/src/claude/adapter.rs",
    "crates/spaghetti-napi/src/codex/adapter.rs",
    "crates/spaghetti-napi/src/grok/adapter.rs",
)
MIGRATED_CLIENT_CONSUMERS = (
    "apps/playground/src/main/canonical-queries.ts",
    "packages/sdk/src/observation-shadow.ts",
)
DIRECT_ENGINE_QUERY_RE = re.compile(
    r"\b(?:this\.)?engine\.(?:"
    r"health|overview|replayChanges|waitForCommit|listHistoryProjects|listHistorySessions|getSession|getMessages|search|getTimeline|"
    r"listDelegations|listWorkflows|getWorkflow|listWorkflowMembers|listMemoryDocuments|listTaskCollections|listTasks|"
    r"listPlans|listToolResults|listArtifacts|listSources|getStats|getUsage|getUsageActivity|getRuntimeSnapshot|"
    r"getRunState|listTeams|getTeam|listTeamInboxes|listTeamInboxMessages"
    r")\s*\("
)
RUNTIME_MODULE_RE = re.compile(
    r"^\s*(?:"
    r"import\s+(?!type\b)(?:[^;'\"]*?\sfrom\s+)?"
    r"|export\s+(?!type\b)[^;'\"]*?\sfrom\s+"
    r")[\'\"]([^\'\"]+)[\'\"]",
    re.MULTILINE,
)
PORTABLE_CLIENT_ENTRY = REPO_ROOT / "packages/sdk/src/client/portable.ts"
OBSERVATION_HOST_ENTRY = REPO_ROOT / "packages/sdk/src/observation-host.ts"
SDK_PRODUCTION_ENTRIES = (
    REPO_ROOT / "packages/sdk/src/index.ts",
    REPO_ROOT / "packages/sdk/src/observation.ts",
    REPO_ROOT / "packages/sdk/src/client/portable.ts",
    REPO_ROOT / "packages/sdk/src/react/index.ts",
)
PORTABLE_FORBIDDEN_EXTERNAL_RE = re.compile(
    r"^(?:node:sqlite|better-sqlite3|chokidar)$"
    r"|^@parcel/watcher(?:$|[-/])"
    r"|^@vibecook/spaghetti-sdk-native(?:$|[-/])"
)
RUST_EXTERNAL_MODULE_RE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;\s*$"
)
RUST_LEGACY_DEFAULT_MODULES = {
    "crates/spaghetti-napi/src/orchestrate/ingest.rs",
    "crates/spaghetti-napi/src/orchestrate/live_ingest.rs",
    "crates/spaghetti-napi/src/core/writer.rs",
    "crates/spaghetti-napi/src/core/token_activity.rs",
    "crates/spaghetti-napi/src/claude/project_parser.rs",
    "crates/spaghetti-napi/src/codex/reader.rs",
    "crates/spaghetti-napi/src/grok/reader.rs",
}
LEGACY_NAPI_DECLARATION_RE = re.compile(
    r"^export declare function (?:ingest|liveIngestBatch)\b", re.MULTILINE
)
REACT_SYNCHRONOUS_BYPASS_RE = re.compile(
    r"\buseSyncExternalStore\b"
    r"|\b(?:api|client)\.live\b"
    r"|\bas\s+unknown\s+as\s+SpaghettiAPI\b"
    r"|\bLegacySpaghettiAPI\b"
    r"|['\"](?:\.\.?/)*legacy-api(?:\.js)?['\"]"
)


def repo_path(path: Path) -> str:
    return path.relative_to(REPO_ROOT).as_posix()


def resolve_typescript_module(importer: Path, specifier: str) -> Path | None:
    if not specifier.startswith("."):
        return None
    unresolved = (importer.parent / specifier).resolve()
    candidates = [unresolved]
    if unresolved.suffix in {".js", ".jsx"}:
        candidates.extend(
            [
                unresolved.with_suffix(".ts"),
                unresolved.with_suffix(".tsx"),
            ]
        )
    elif not unresolved.suffix:
        candidates.extend(
            [
                unresolved.with_suffix(".ts"),
                unresolved.with_suffix(".tsx"),
                unresolved / "index.ts",
                unresolved / "index.tsx",
            ]
        )
    for candidate in candidates:
        if candidate.exists() and candidate.suffix in {".ts", ".tsx"}:
            return candidate.resolve()
    return None


def production_typescript() -> list[Path]:
    """Runtime-reachable SDK sources shipped by declared package entries.

    The old TypeScript engine remains in-tree only as a repository differential
    oracle. Package exports cannot reach it, so scanning every historical file
    would confuse test tooling with production authority and would not model
    the bundles RFC 011 actually constrains.
    """
    root = (REPO_ROOT / "packages/sdk/src").resolve()
    pending = list(SDK_PRODUCTION_ENTRIES)
    visited: set[Path] = set()
    while pending:
        path = pending.pop().resolve()
        if path in visited or not path.is_relative_to(root):
            continue
        visited.add(path)
        for specifier in RUNTIME_MODULE_RE.findall(read(path)):
            target = resolve_typescript_module(path, specifier)
            if target is not None and target.is_relative_to(root):
                pending.append(target)
    return sorted(visited)


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


def production_rust() -> list[Path]:
    """Rust modules reachable from lib.rs with default crate features.

    This deliberately models module declarations instead of scanning every
    repository oracle file. A legacy module becomes visible to this walk if
    its `cfg(feature = "legacy-oracle")` guard is ever removed.
    """
    crate_root = (REPO_ROOT / "crates/spaghetti-napi/src").resolve()
    pending = [crate_root / "lib.rs"]
    visited: set[Path] = set()
    while pending:
        path = pending.pop().resolve()
        if path in visited or not path.exists() or not path.is_relative_to(crate_root):
            continue
        visited.add(path)
        attributes: list[str] = []
        for line in read(path).splitlines():
            stripped = line.strip()
            if stripped.startswith("#["):
                attributes.append(stripped)
                continue
            match = RUST_EXTERNAL_MODULE_RE.match(line)
            if match:
                guarded = " ".join(attributes)
                attributes = []
                if 'feature = "legacy-oracle"' in guarded or "cfg(test)" in guarded:
                    continue
                name = match.group(1)
                if path.name in {"lib.rs", "main.rs", "mod.rs"}:
                    base = path.parent
                else:
                    base = path.parent / path.stem
                candidates = (base / f"{name}.rs", base / name / "mod.rs")
                target = next((candidate for candidate in candidates if candidate.exists()), None)
                if target is not None:
                    pending.append(target)
                continue
            if stripped and not stripped.startswith("//"):
                attributes = []
    return sorted(visited)


def discover_rust_common_source_dispatch() -> set[str]:
    root = REPO_ROOT / "crates/spaghetti-napi/src"
    adapter_roots = {"claude", "codex", "grok"}
    found: set[str] = set()
    for path in production_rust():
        relative = path.relative_to(root)
        if relative.parts[0] in adapter_roots:
            continue
        text = production_rust_text(path)
        # The N-API module is the current compiled-adapter composition root and
        # compatibility facade. Common engine/source/adapter modules must not
        # depend on concrete adapters even when an identifier is assembled in
        # a way that evades a plain quoted-literal search.
        is_host_composition = relative.as_posix() == "napi_engine.rs"
        is_common_runtime = relative.parts[0] in {"adapter", "engine", "source"}
        if (
            (not is_host_composition and SOURCE_ID_LITERAL_RE.search(text))
            or (is_common_runtime and SOURCE_ID_CONCAT_RE.search(text))
            or (is_common_runtime and CONCRETE_ADAPTER_DEPENDENCY_RE.search(text))
        ):
            found.add(repo_path(path))
    return found


def discover_rust_legacy_oracle_default_exposure() -> set[str]:
    """The retired writer may compile only behind an explicit test feature."""
    found: set[str] = set()
    cargo_path = REPO_ROOT / "crates/spaghetti-napi/Cargo.toml"
    manifest = tomllib.loads(read(cargo_path))
    features = manifest.get("features", {})
    if features.get("default") != [] or "legacy-oracle" not in features:
        found.add(f"{repo_path(cargo_path)}#features")
    reachable = {repo_path(path) for path in production_rust()}
    found.update(reachable & RUST_LEGACY_DEFAULT_MODULES)
    declarations = REPO_ROOT / "crates/spaghetti-napi/index.d.ts"
    if declarations.exists() and LEGACY_NAPI_DECLARATION_RE.search(read(declarations)):
        found.add(f"{repo_path(declarations)}#legacy-napi-export")
    return found


def discover_rust_source_boundary_violations() -> set[str]:
    root = REPO_ROOT / "crates/spaghetti-napi/src/source"
    if not root.exists():
        return set()
    found: set[str] = set()
    for path in sorted(root.rglob("*.rs")):
        relative = repo_path(path)
        text = production_rust_text(path)
        if SOURCE_LAYER_FORBIDDEN_RE.search(text) or (
            SOURCE_LAYER_SQLITE_RE.search(text)
            and relative != APPROVED_SOURCE_SQLITE_DRIVER
        ):
            found.add(relative)
    return found


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


def discover_rfc012_adapter_access_authority_violations() -> set[str]:
    """Adapters may declare bounds but cannot reserve or mint native access."""
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
        if RFC012_ADAPTER_ACCESS_AUTHORITY_RE.search(production_rust_text(path))
    }


def discover_rfc012_semantic_boundary_violations() -> set[str]:
    """The RFC 012A base model cannot depend on adapters, sources, or topology."""
    path = REPO_ROOT / "crates/spaghetti-napi/src/adapter/semantic.rs"
    if not path.exists() or RFC012_SEMANTIC_FORBIDDEN_RE.search(production_rust_text(path)):
        return {repo_path(path)}
    return set()


def discover_rfc012_support_boundary_violations() -> set[str]:
    """Support selection may inspect declarations, never sources or runtime authority."""
    path = REPO_ROOT / "crates/spaghetti-napi/src/adapter/support.rs"
    if not path.exists():
        return {repo_path(path)}
    text = production_rust_text(path).replace("super::scope::", "")
    if RFC012_SUPPORT_FORBIDDEN_RE.search(text):
        return {repo_path(path)}
    return set()


def discover_rfc012_adapter_support_binding_gaps() -> set[str]:
    """Built-in adapters must bind and compile their support declarations."""
    found: set[str] = set()
    for relative in BUILTIN_ADAPTER_PATHS:
        text = production_rust_text(REPO_ROOT / relative)
        if (
            "support_binding: Some(" not in text
            or "AdapterSupportBinding::new(" not in text
            or "scope_programs: Some(" not in text
            or "ScopeProgramManifest::from_json(" not in text
        ):
            found.add(relative)
    return found


def discover_migrated_client_direct_engine_queries() -> set[str]:
    """Once a consumer moves to SpaghettiClient, direct N-API reads cannot return."""
    return {
        relative
        for relative in MIGRATED_CLIENT_CONSUMERS
        if DIRECT_ENGINE_QUERY_RE.search(read(REPO_ROOT / relative))
    }


def discover_portable_client_runtime_boundary_violations() -> set[str]:
    """The Electron/client entry cannot pull storage, watchers, or N-API at runtime."""
    client_root = PORTABLE_CLIENT_ENTRY.parent.resolve()
    pending = [PORTABLE_CLIENT_ENTRY]
    visited: set[Path] = set()
    violations: set[str] = set()
    while pending:
        path = pending.pop().resolve()
        if path in visited:
            continue
        visited.add(path)
        for specifier in RUNTIME_MODULE_RE.findall(read(path)):
            edge = f"{repo_path(path)} -> {specifier}"
            if not specifier.startswith("."):
                if PORTABLE_FORBIDDEN_EXTERNAL_RE.search(specifier):
                    violations.add(edge)
                continue
            target = (path.parent / specifier).resolve()
            if target.suffix in {".js", ".jsx"}:
                target = target.with_suffix(".ts" if target.suffix == ".js" else ".tsx")
            if not target.is_relative_to(client_root) or not target.exists():
                violations.add(edge)
                continue
            pending.append(target)
    return violations


def discover_playground_main_sdk_owner_bypasses() -> set[str]:
    """Electron brokers may use portable clients but cannot load the owner SDK."""
    found: set[str] = set()
    for directory in ("apps/playground/src/main", "apps/playground/src/preload"):
        for path in sorted((REPO_ROOT / directory).rglob("*.ts")):
            if "__tests__" in path.parts:
                continue
            if "@vibecook/spaghetti-sdk" in RUNTIME_MODULE_RE.findall(read(path)):
                found.add(repo_path(path))
    return found


def discover_observation_host_runtime_boundary_violations() -> set[str]:
    """The native observation owner cannot regain a TypeScript ingest plane."""
    sdk_root = (REPO_ROOT / "packages/sdk/src").resolve()
    client_root = (sdk_root / "client").resolve()
    native_entry = (sdk_root / "native.ts").resolve()
    settings_entry = (sdk_root / "settings.ts").resolve()
    pending = [OBSERVATION_HOST_ENTRY]
    visited: set[Path] = set()
    violations: set[str] = set()
    while pending:
        path = pending.pop().resolve()
        if path in visited:
            continue
        visited.add(path)
        for specifier in RUNTIME_MODULE_RE.findall(read(path)):
            edge = f"{repo_path(path)} -> {specifier}"
            if not specifier.startswith("."):
                if PORTABLE_FORBIDDEN_EXTERNAL_RE.search(specifier) and not specifier.startswith(
                    "@vibecook/spaghetti-sdk-native"
                ):
                    violations.add(edge)
                continue
            target = (path.parent / specifier).resolve()
            if target.suffix in {".js", ".jsx"}:
                target = target.with_suffix(".ts" if target.suffix == ".js" else ".tsx")
            allowed_target = target in {native_entry, settings_entry} or target.is_relative_to(client_root)
            if not target.is_relative_to(sdk_root) or not target.exists() or not allowed_target:
                violations.add(edge)
                continue
            pending.append(target)
    return violations


def discover_react_synchronous_query_bypasses() -> set[str]:
    """Published React and Electron renderer code must consume async reads."""
    roots = (
        REPO_ROOT / "packages/sdk/src/react",
        REPO_ROOT / "apps/playground/src/renderer/src",
    )
    found: set[str] = set()
    for root in roots:
        for path in sorted((*root.rglob("*.ts"), *root.rglob("*.tsx"))):
            if "__tests__" in path.parts:
                continue
            if REACT_SYNCHRONOUS_BYPASS_RE.search(read(path)):
                found.add(repo_path(path))
    return found


def discover_sdk_package_boundary_gaps() -> set[str]:
    """The package build must roll entry declarations and scan the artifact."""
    found: set[str] = set()
    vite_config = REPO_ROOT / "packages/sdk/vite.config.ts"
    package_path = REPO_ROOT / "packages/sdk/package.json"
    checker = REPO_ROOT / "scripts/check-sdk-package.mjs"
    if not re.search(r"\brollupTypes\s*:\s*true\b", read(vite_config)):
        found.add(f"{repo_path(vite_config)}#rollupTypes")
    package_manifest = json.loads(read(package_path))
    if "check-sdk-package.mjs" not in package_manifest.get("scripts", {}).get("build", ""):
        found.add(f"{repo_path(package_path)}#build")
    if not checker.exists():
        found.add(repo_path(checker))
    return found


DISCOVERERS: dict[str, Callable[[], set[str]]] = {
    "typescript_sql_authorities": discover_typescript_sql_authorities,
    "typescript_sql_drivers": discover_typescript_sql_drivers,
    "typescript_query_projection_mutators": discover_query_projection_mutators,
    "source_specific_runtime_services": discover_source_runtime_services,
    "rust_common_source_dispatch": discover_rust_common_source_dispatch,
    "rust_legacy_oracle_default_exposure": discover_rust_legacy_oracle_default_exposure,
    "rust_source_boundary_violations": discover_rust_source_boundary_violations,
    "rust_adapter_storage_boundary_violations": discover_rust_adapter_storage_boundary_violations,
    "rfc012_adapter_access_authority_violations": discover_rfc012_adapter_access_authority_violations,
    "rfc012_semantic_boundary_violations": discover_rfc012_semantic_boundary_violations,
    "rfc012_support_boundary_violations": discover_rfc012_support_boundary_violations,
    "rfc012_adapter_support_binding_gaps": discover_rfc012_adapter_support_binding_gaps,
    "migrated_client_direct_engine_queries": discover_migrated_client_direct_engine_queries,
    "portable_client_runtime_boundary_violations": discover_portable_client_runtime_boundary_violations,
    "playground_main_sdk_owner_bypasses": discover_playground_main_sdk_owner_bypasses,
    "observation_host_runtime_boundary_violations": discover_observation_host_runtime_boundary_violations,
    "react_synchronous_query_bypasses": discover_react_synchronous_query_bypasses,
    "sdk_package_boundary_gaps": discover_sdk_package_boundary_gaps,
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
