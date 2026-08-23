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
SOURCE_ID_LITERAL_RE = re.compile(r'"(?:claude-code|codex|factory|grok)"')
SOURCE_ID_CONCAT_RE = re.compile(
    r"\bconcat!\s*\([^)]*\b(?:claude|codex|factory|grok)\b[^)]*\)"
)
CONCRETE_ADAPTER_DEPENDENCY_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|factory|grok)(?:::|\b)"
    r"|(?:ClaudeCode|Codex|Factory|Grok)Adapter\b)"
)
SOURCE_LAYER_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|factory|grok|engine|orchestrate|napi_engine)"
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
RFC012_ADAPTER_ARTIFACT_PROBE_RE = re.compile(
    r"\bfn\s+native_support_probe\b|\bsupport_probe::|\bprobe_[a-z0-9_]*native_artifact\b"
)
RFC012_SEMANTIC_FORBIDDEN_RE = re.compile(
    r"(?:\bcrate::|\bsuper::(?:contract|facts|registry)(?:::|\b)"
    r"|\brusqlite(?:::|\b)|\bnapi(?:::|\b))"
)
RFC012_SUPPORT_FORBIDDEN_RE = re.compile(
    r"(?:\bcrate::|\bsuper::(?:::|\b)|\brusqlite(?:::|\b)|\bnapi(?:::|\b))"
)
RFC012_OBSERVER_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|factory|grok|core|engine|napi_engine|orchestrate)"
    r"|rusqlite|napi)(?:::|\b)"
)
RFC012_OBSERVATION_CONTRACT_FORBIDDEN_RE = re.compile(
    r"(?:\bcrate::(?!adapter(?:::|\b))|\brusqlite(?:::|\b)|\bnapi(?:::|\b))"
)
RFC012_DECODE_RUNTIME_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|factory|grok|core|engine|napi_engine|orchestrate|observer)"
    r"|rusqlite|napi)(?:::|\b)"
)
RFC012_SEMANTIC_CONTRACT_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|factory|grok|core|engine|napi_engine|orchestrate|scoped_observation|source|catalog_contract|observation_contract)"
    r"|rusqlite|napi)(?:::|\b)"
)
RFC012_SEMANTIC_CONTRACT_NAPI_FORBIDDEN_RE = re.compile(
    r"\b(?:crate::(?:claude|codex|factory|grok|core|engine|napi_engine|orchestrate|scoped_observation|source|catalog_contract|observation_contract)"
    r"|rusqlite)(?:::|\b)"
)
BUILTIN_ADAPTER_PATHS = (
    "crates/spaghetti-napi/src/claude/adapter.rs",
    "crates/spaghetti-napi/src/codex/adapter.rs",
    "crates/spaghetti-napi/src/factory/adapter.rs",
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
PORTABLE_CLIENT_CONTRACT_MODULES = frozenset(
    (REPO_ROOT / relative).resolve()
    for relative in (
        "packages/sdk/src/contracts/rfc012-semantic-json.ts",
        "packages/sdk/src/contracts/rfc012a.ts",
    )
)
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
    adapter_roots = {"claude", "codex", "factory", "grok"}
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
    root = (REPO_ROOT / "crates/spaghetti-napi/src/source").resolve()
    if not root.exists():
        return set()
    found: set[str] = set()
    # Scan only default-build modules. In particular, a `#[cfg(test)] mod
    # tests;` child may use fixture tooling such as serde_json without adding
    # that dependency to the production source layer. The production_rust()
    # walk retains this distinction while still discovering every reachable
    # source module recursively.
    for path in (
        path for path in production_rust() if path.is_relative_to(root)
    ):
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
        for adapter in ("claude", "codex", "factory", "grok")
        if (path := root / adapter / "adapter.rs").exists()
    )
    return {
        repo_path(path)
        for path in sorted(paths)
        if ADAPTER_STORAGE_FORBIDDEN_RE.search(production_rust_text(path))
    }


def discover_rfc012_adapter_access_authority_violations() -> set[str]:
    """Adapters may declare bounds but cannot probe, reserve, or mint native access."""
    root = REPO_ROOT / "crates/spaghetti-napi/src"
    paths = list((root / "adapter").rglob("*.rs"))
    concrete_adapters = [
        path
        for adapter in ("claude", "codex", "factory", "grok")
        if (path := root / adapter / "adapter.rs").exists()
    ]
    paths.extend(concrete_adapters)
    found = {
        repo_path(path)
        for path in sorted(paths)
        if RFC012_ADAPTER_ACCESS_AUTHORITY_RE.search(production_rust_text(path))
    }
    contract = root / "adapter" / "contract.rs"
    for path in [contract, *concrete_adapters]:
        if RFC012_ADAPTER_ARTIFACT_PROBE_RE.search(production_rust_text(path)):
            found.add(repo_path(path))
    return found


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


def discover_rfc012_scoped_host_boundary_violations() -> set[str]:
    """The store-free observer holds no persistence, query, or vendor authority.

    RFC 012D section 2: a scoped observer opens no store, migration, query pool,
    durable outbox, or configured whole-adapter host, and it composes the same
    decode spine and semantic reducers the durable host uses. The N-API binding
    layer is the one exception: it is the composition root, so it is allowed to
    resolve the adapter that every other module then treats as opaque.
    """
    relative = "crates/spaghetti-napi/src/observer"
    root = REPO_ROOT / relative
    found: set[str] = set()
    modules = [
        candidate for candidate in production_rust() if candidate.is_relative_to(root)
    ]
    if not root.is_dir() or not modules:
        found.add(relative)
        return found
    binding_layer = root / "bindings.rs"
    for module in modules:
        if module == binding_layer:
            continue
        text = production_rust_text(module)
        if RFC012_OBSERVER_FORBIDDEN_RE.search(text):
            found.add(f"{repo_path(module)}#observer-acquired-foreign-authority")
    # One decode spine, shared reducers: the observer is not a second parser.
    if "decode_record(DecodeRuntimeRequest" not in production_rust_text(root / "object.rs"):
        found.add(f"{relative}/object.rs#missing-shared-decode-spine")
    state_text = production_rust_text(root / "state.rs")
    if any(
        binding not in state_text
        for binding in (
            "reduce_runtime_fact_revision(",
            "runtime_replacement_state_digest(",
        )
    ):
        found.add(f"{relative}/state.rs#missing-shared-reducer-law")
    # Deterministic identity and epoch replacement are semantic contracts.
    identity_text = production_rust_text(root / "identity.rs")
    if "source_record_id" not in identity_text:
        found.add(f"{relative}/identity.rs#event-id-missing-source-occurrence")
    queue_text = production_rust_text(root / "queue.rs")
    if any(
        binding not in queue_text
        for binding in ("fn invalidate_epoch", "fn begin_replacement_epoch", "fn admit_control")
    ):
        found.add(f"{relative}/queue.rs#missing-epoch-replacement-or-control-lane")
    return found

def discover_rfc012_decode_runtime_boundary_violations() -> set[str]:
    """The shared decoder boundary cannot depend on a sink or concrete adapter."""
    relative = "crates/spaghetti-napi/src/decode_runtime.rs"
    path = REPO_ROOT / relative
    text = production_rust_text(path) if path.exists() else ""
    required_bindings = (
        "pub(crate) fn bootstrap_object_without_source_access",
        "adapter.bootstrap_object_without_source_access(instance, object)",
        "catch_unwind(AssertUnwindSafe",
    )
    if (
        not path.exists()
        or RFC012_DECODE_RUNTIME_FORBIDDEN_RE.search(text)
        or any(binding not in text for binding in required_bindings)
    ):
        return {relative}
    return set()


def discover_rfc012_semantic_contract_boundary_violations() -> set[str]:
    """RFC 012A/012C fixture JSON stays store-free; N-API helpers stay JSON-string only."""
    found: set[str] = set()
    contract_relative = "crates/spaghetti-napi/src/semantic_contract.rs"
    napi_relative = "crates/spaghetti-napi/src/semantic_contract_napi.rs"
    contract_path = REPO_ROOT / contract_relative
    napi_path = REPO_ROOT / napi_relative
    if not contract_path.exists() or RFC012_SEMANTIC_CONTRACT_FORBIDDEN_RE.search(
        production_rust_text(contract_path)
    ):
        found.add(contract_relative)
    napi_text = production_rust_text(napi_path) if napi_path.exists() else ""
    napi_normalized = re.sub(r"\s+", " ", napi_text)
    napi_helpers = re.findall(
        r"pub fn (parse_[A-Za-z0-9_]+)\(([^)]*)\)\s*->\s*([^ {]+)",
        napi_text,
    )
    required_helpers = {
        ("parse_rfc012a_v1_json", "json: Utf16String", "Result<String>"),
        ("parse_rfc012c_runtime_v1_json", "json: Utf16String", "Result<String>"),
        ("parse_rfc012c_effective_state_v1_json", "json: Utf16String", "Result<String>"),
        ("parse_rfc012c_interaction_v1_json", "json: Utf16String", "Result<String>"),
        ("parse_rfc012c_message_v1_json", "json: Utf16String", "Result<String>"),
        ("parse_rfc012c_native_marker_v1_json", "json: Utf16String", "Result<String>"),
        ("parse_rfc012c_task_v1_json", "json: Utf16String", "Result<String>"),
        ("parse_rfc012c_plan_v1_json", "json: Utf16String", "Result<String>"),
        ("parse_rfc012c_tool_v1_json", "json: Utf16String", "Result<String>"),
    }
    required_js_names = {
        "parseRfc012aV1Json",
        "parseRfc012cRuntimeV1Json",
        "parseRfc012cEffectiveStateV1Json",
        "parseRfc012cInteractionV1Json",
        "parseRfc012cMessageV1Json",
        "parseRfc012cNativeMarkerV1Json",
        "parseRfc012cTaskV1Json",
        "parseRfc012cPlanV1Json",
        "parseRfc012cToolV1Json",
    }
    napi_attr_count = len(re.findall(r"^#\[napi", napi_text, re.MULTILINE))
    declarations = REPO_ROOT / "crates/spaghetti-napi/index.d.ts"
    declared_helpers = (
        re.findall(
            r"^export declare function (parseRfc012[A-Za-z0-9]*)\(([^)]*)\):\s*(\S+)",
            read(declarations),
            re.MULTILINE,
        )
        if declarations.exists()
        else []
    )
    required_declared_helpers = {
        ("parseRfc012aV1Json", "json: string", "string"),
        ("parseRfc012cRuntimeV1Json", "json: string", "string"),
        ("parseRfc012cEffectiveStateV1Json", "json: string", "string"),
        ("parseRfc012cInteractionV1Json", "json: string", "string"),
        ("parseRfc012cMessageV1Json", "json: string", "string"),
        ("parseRfc012cNativeMarkerV1Json", "json: string", "string"),
        ("parseRfc012cTaskV1Json", "json: string", "string"),
        ("parseRfc012cPlanV1Json", "json: string", "string"),
        ("parseRfc012cToolV1Json", "json: string", "string"),
    }
    if (
        not napi_path.exists()
        or RFC012_SEMANTIC_CONTRACT_NAPI_FORBIDDEN_RE.search(napi_text)
        or re.search(r"^#\[napi\(object\)\]", napi_text, re.MULTILINE)
        or any(f'js_name = "{name}"' not in napi_text for name in required_js_names)
        or "pub fn parse_rfc012a_v1_json(json: Utf16String) -> Result<String>"
        not in napi_normalized
        or "pub fn parse_rfc012c_runtime_v1_json(json: Utf16String) -> Result<String>"
        not in napi_normalized
        or "String::from_utf16" not in napi_text
        or "MAX_SEMANTIC_FIXTURE_JSON_BYTES" not in napi_text
        or "json.len() > MAX_SEMANTIC_FIXTURE_JSON_BYTES" not in napi_normalized
        or napi_text.find("json.len()") > napi_text.find("String::from_utf16")
        or "invalid semantic fixture: unknown field" not in napi_text
        or napi_attr_count != len(required_helpers)
        or set(napi_helpers) != required_helpers
        or set(declared_helpers) != required_declared_helpers
    ):
        found.add(napi_relative)
    lib = REPO_ROOT / "crates/spaghetti-napi/src/lib.rs"
    lib_text = read(lib)
    if re.search(r"^\s*pub\s+mod\s+semantic_contract(?:_napi)?\s*;", lib_text, re.MULTILINE):
        found.add(f"{repo_path(lib)}#premature-public-semantic-contract")
    if not re.search(r"^\s*mod\s+semantic_contract\s*;", lib_text, re.MULTILINE):
        found.add(f"{repo_path(lib)}#missing-semantic-contract")
    if not re.search(r"^\s*mod\s+semantic_contract_napi\s*;", lib_text, re.MULTILINE):
        found.add(f"{repo_path(lib)}#missing-semantic-contract-napi")
    portable_relatives = (
        "packages/sdk/src/contracts/rfc012a.ts",
        "packages/sdk/src/contracts/rfc012c.ts",
    )
    for relative in portable_relatives:
        text = read(REPO_ROOT / relative)
        if "@vibecook/spaghetti-sdk-native" in text:
            found.add(relative)
    rfc012a = read(REPO_ROOT / "packages/sdk/src/contracts/rfc012a.ts")
    qualified_start = rfc012a.find("export function parseQualifiedValue")
    qualified_end = rfc012a.find("export function parseNativeIdentityClaim")
    qualified_body = rfc012a[qualified_start:qualified_end]
    if (
        qualified_start < 0
        or qualified_end < 0
        or "assertKnownFields(" not in qualified_body
        or "unknown_reason cannot be explicit null" not in qualified_body
        or "parseKnownValue" not in qualified_body
        or "parseAuthority" not in qualified_body
        or "parseProvenance" not in qualified_body
        or "Array.isArray(provenance)" in qualified_body
        or "Array.isArray(input.provenance)" in qualified_body
    ):
        found.add("packages/sdk/src/contracts/rfc012a.ts#parseQualifiedValue-unknown-fields")
    if "export function parseRfc012aV1Fixture(" not in rfc012a:
        found.add("packages/sdk/src/contracts/rfc012a.ts#missing-typed-fixture-consumer")
    if (
        "export function preflightSemanticFixtureJson(" in rfc012a
        or "export function hasSurroundingRustWhitespace(" in rfc012a
        or "export function assertNoUnpairedUtf16Surrogates(" in rfc012a
    ):
        found.add("packages/sdk/src/contracts/rfc012a.ts#leaked-internal-helpers")
    semantic_json_relative = "packages/sdk/src/contracts/rfc012-semantic-json.ts"
    semantic_json = read(REPO_ROOT / semantic_json_relative)
    if (
        "export function preflightSemanticFixtureJson(" not in semantic_json
        or "MAX_SEMANTIC_FIXTURE_JSON_BYTES" not in semantic_json
        or "MAX_SEMANTIC_FIXTURE_DEPTH" not in semantic_json
        or "MAX_SEMANTIC_FIXTURE_NODES" not in semantic_json
        or "json.length > MAX_SEMANTIC_FIXTURE_JSON_BYTES" not in semantic_json
        or "noncanonical integer lexeme" not in semantic_json
        or "unpaired UTF-16 surrogate" not in semantic_json
        or "hasSurroundingRustWhitespace" not in semantic_json
        or "Object.hasOwn(record, key)" not in semantic_json
    ):
        found.add(f"{semantic_json_relative}#missing-fixture-envelope")
    if "./contracts/rfc012-semantic-json.js" in read(
        REPO_ROOT / "packages/sdk/src/index.ts"
    ):
        found.add("packages/sdk/src/index.ts#barreled-semantic-json-helpers")
    if (
        "qualified unknown provenance must remain empty" not in rfc012a
        or "native identity provenance must bind the fixture semantic revision reference"
        not in rfc012a
    ):
        found.add("packages/sdk/src/contracts/rfc012a.ts#missing-typed-fixture-consumer")
    rfc012c = read(REPO_ROOT / "packages/sdk/src/contracts/rfc012c.ts")
    if (
        "assertKnownFields(" not in rfc012c
        or "must be a plain object" not in rfc012c
        or "runtime fixture families must be actor-run, actor-affiliation, and usage-v2 v1"
        not in rfc012c
        or "workflow removal must mint a distinct semantic revision" not in rfc012c
        or "fixture usage revisions must reference a fixture actor" not in rfc012c
        or "expectedContextInput: unknown" not in rfc012c
        or "caller-held revision identity" not in rfc012c
        or "exceeds u32" not in rfc012c
        or "fixture examples must share one source-record identity" not in rfc012c
        or "parsed.source_record_id !== expected.source_record_id" not in rfc012c
        or "MAX_ADAPTER_ID_BYTES" not in rfc012c
    ):
        found.add("packages/sdk/src/contracts/rfc012c.ts#missing-unknown-field-rejection")
    contract_text = production_rust_text(contract_path) if contract_path.exists() else ""
    if (
        "pub(crate) const MAX_SEMANTIC_FIXTURE_JSON_BYTES" not in contract_text
        or "json.len() > MAX_SEMANTIC_FIXTURE_JSON_BYTES" not in contract_text
        or "MAX_SEMANTIC_FIXTURE_DEPTH" not in contract_text
        or "MAX_SEMANTIC_FIXTURE_NODES" not in contract_text
        or "fn strict_actor_run_revision" not in contract_text
        or "fn strict_usage_revision" not in contract_text
        or "fn validate_canonical_source_string" not in contract_text
        or "workflow removal must mint a distinct semantic revision" not in contract_text
        or "fixture usage revisions must reference a fixture actor" not in contract_text
        or "native identity provenance must bind the fixture semantic revision reference"
        not in contract_text
        or "qualified unknown provenance must remain empty" not in contract_text
        or "fixture examples must share one source-record identity" not in contract_text
        or "response_key exceeds the bounded encoded base64 maximum" not in contract_text
        or "MAX_ADAPTER_ID_BYTES" not in contract_text
        or "MAX_AUTHORITY_BYTES" not in contract_text
    ):
        found.add(f"{contract_relative}#missing-strict-fixture-graph")
    semantic_text = production_rust_text(
        REPO_ROOT / "crates/spaghetti-napi/src/adapter/semantic.rs"
    )
    if 'deserialize_with = "deserialize_present_non_null"' not in semantic_text or (
        "unknown_reason: Option<QualifiedUnknownReason>" not in semantic_text
    ):
        found.add(
            "crates/spaghetti-napi/src/adapter/semantic.rs#qualified-value-explicit-nulls"
        )
    if "validate_identifier(\"qualified value authority\"" not in semantic_text:
        found.add(
            "crates/spaghetti-napi/src/adapter/semantic.rs#qualified-authority-canonical"
        )
    return found


def discover_rfc012_coverage_crate_boundary_violations() -> set[str]:
    """Physical spaghetti-coverage crate must own membership encoding without store/N-API."""
    found: set[str] = set()
    crate_root = REPO_ROOT / "crates/spaghetti-coverage/src"
    if not crate_root.exists():
        found.add("crates/spaghetti-coverage/src")
        return found
    forbidden = re.compile(r"\b(?:rusqlite|napi|crate::(?:engine|claude|codex|grok|factory))\b")
    for path in crate_root.rglob("*.rs"):
        if forbidden.search(production_rust_text(path)):
            found.add(repo_path(path))
    cargo = REPO_ROOT / "crates/spaghetti-coverage/Cargo.toml"
    cargo_text = cargo.read_text(encoding="utf-8") if cargo.exists() else ""
    if re.search(r"(?m)^\s*(?:rusqlite|napi|spaghetti-napi)\s*=", cargo_text):
        found.add("crates/spaghetti-coverage/Cargo.toml")
    wrapper = REPO_ROOT / "crates/spaghetti-napi/src/coverage_runtime.rs"
    wrapper_text = production_rust_text(wrapper) if wrapper.exists() else ""
    if "spaghetti_coverage::membership_digest" not in wrapper_text:
        found.add("crates/spaghetti-napi/src/coverage_runtime.rs#missing-coverage-crate")
    if "CoverageMembershipRevision::begin_streaming" in wrapper_text:
        found.add("crates/spaghetti-napi/src/coverage_runtime.rs#reimplemented-membership-encoder")
    return found


def discover_rfc012_catalog_contract_boundary_violations() -> set[str]:
    """The catalog lives behind the Rust engine boundary and nowhere else.

    RFC 012B's catalog was once a draft semantic contract mirrored into
    hand-written TypeScript. It is now durable engine rows reached through the
    generated N-API bindings, so any reappearance of a separate catalog
    contract module — in either language — is a regression to two authorities.
    """
    found: set[str] = set()
    rust_contract = REPO_ROOT / "crates/spaghetti-napi/src/catalog_contract.rs"
    if rust_contract.exists() or rust_contract.with_suffix("").exists():
        found.add(repo_path(rust_contract))
    contracts_dir = REPO_ROOT / "packages/sdk/src/contracts"
    if contracts_dir.exists():
        for module in sorted(contracts_dir.glob("rfc012b*.ts")):
            found.add(repo_path(module))
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
            if (
                not target.exists()
                or not (
                    target.is_relative_to(client_root)
                    or target in PORTABLE_CLIENT_CONTRACT_MODULES
                )
            ):
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
            allowed_target = (
                target in {native_entry, settings_entry}
                or target.is_relative_to(client_root)
                or target in PORTABLE_CLIENT_CONTRACT_MODULES
            )
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
    "rfc012_scoped_host_boundary_violations": discover_rfc012_scoped_host_boundary_violations,
    "rfc012_decode_runtime_boundary_violations": discover_rfc012_decode_runtime_boundary_violations,
    "rfc012_catalog_contract_boundary_violations": discover_rfc012_catalog_contract_boundary_violations,
    "rfc012_coverage_crate_boundary_violations": discover_rfc012_coverage_crate_boundary_violations,
    "rfc012_semantic_contract_boundary_violations": discover_rfc012_semantic_contract_boundary_violations,
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
