#!/usr/bin/env python3
"""Validate RFC 012A ADS, source/scope, evidence, and support-release bundles."""

from __future__ import annotations

import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Mapping

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from scripts.agent_support.contracts import validate_numeric_ranges
from scripts.agent_support.sanitize_fixture import (
    SANITIZER_VERSION,
    scan_fixture_file,
)


REPO_ROOT = Path(__file__).resolve().parents[2]
SUPPORT_ROOT = REPO_ROOT / "agent-support"
SCHEMA_ROOT = SUPPORT_ROOT / "schemas"

DOCUMENT_SCHEMAS = {
    "ads.json": "agent-data-surface.schema.json",
    "source-declarations.json": "source-declaration.schema.json",
    "scope-programs.json": "scope-program.schema.json",
    "evidence.json": "evidence-manifest.schema.json",
    "conformance.json": "conformance-manifest.schema.json",
    "support-release.json": "support-release.schema.json",
}

REQUIRED_CONFORMANCE_CHECKS = {
    "contract-negotiation",
    "cross-topology-parity",
    "family-disposition",
    "identity-determinism",
    "sanitizer",
    "schema-validation",
    "scope-access",
    "source-bounds",
    "tier-compositionality",
    "unknown-retention",
    "version-classifier",
}


def _json_type_matches(value: Any, expected: str) -> bool:
    if expected == "null":
        return value is None
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    return False


def _resolve_local_ref(root_schema: Mapping[str, Any], reference: str) -> Mapping[str, Any]:
    if not reference.startswith("#/"):
        raise ValueError(f"only local JSON Schema references are supported: {reference}")
    current: Any = root_schema
    for raw_part in reference[2:].split("/"):
        part = raw_part.replace("~1", "/").replace("~0", "~")
        current = current[part]
    if not isinstance(current, dict):
        raise ValueError(f"JSON Schema reference is not an object: {reference}")
    return current


def validate_json_schema(
    value: Any,
    schema: Mapping[str, Any],
    *,
    root_schema: Mapping[str, Any] | None = None,
    path: str = "$",
) -> list[str]:
    """Validate the JSON Schema subset used by the RFC 012A schemas."""

    root = schema if root_schema is None else root_schema
    if "$ref" in schema:
        return validate_json_schema(value, _resolve_local_ref(root, schema["$ref"]), root_schema=root, path=path)

    if "oneOf" in schema:
        branch_errors = [
            validate_json_schema(value, branch, root_schema=root, path=path)
            for branch in schema["oneOf"]
        ]
        matching = sum(not errors for errors in branch_errors)
        if matching != 1:
            return [f"{path}: expected exactly one oneOf branch, matched {matching}"]
        return []

    errors: list[str] = []
    expected_type = schema.get("type")
    if expected_type is not None:
        expected_types = [expected_type] if isinstance(expected_type, str) else expected_type
        if not any(_json_type_matches(value, item) for item in expected_types):
            return [f"{path}: expected type {expected_types}, got {type(value).__name__}"]

    if "const" in schema and value != schema["const"]:
        errors.append(f"{path}: expected constant {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{path}: value {value!r} is outside enum {schema['enum']!r}")

    if isinstance(value, dict):
        required = schema.get("required", [])
        for key in required:
            if key not in value:
                errors.append(f"{path}: missing required property {key!r}")
        if len(value) < schema.get("minProperties", 0):
            errors.append(f"{path}: has fewer than {schema['minProperties']} properties")
        properties = schema.get("properties", {})
        additional = schema.get("additionalProperties", True)
        for key, child in value.items():
            child_path = f"{path}.{key}"
            if key in properties:
                errors.extend(validate_json_schema(child, properties[key], root_schema=root, path=child_path))
            elif additional is False:
                errors.append(f"{child_path}: additional property is forbidden")
            elif isinstance(additional, dict):
                errors.extend(validate_json_schema(child, additional, root_schema=root, path=child_path))

    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0):
            errors.append(f"{path}: has fewer than {schema['minItems']} items")
        if schema.get("uniqueItems"):
            encoded = [json.dumps(item, ensure_ascii=False, sort_keys=True, separators=(",", ":")) for item in value]
            if len(encoded) != len(set(encoded)):
                errors.append(f"{path}: items are not unique")
        item_schema = schema.get("items")
        if isinstance(item_schema, dict):
            for index, child in enumerate(value):
                errors.extend(validate_json_schema(child, item_schema, root_schema=root, path=f"{path}[{index}]"))

    if isinstance(value, str):
        if len(value) < schema.get("minLength", 0):
            errors.append(f"{path}: string is shorter than {schema['minLength']}")
        pattern = schema.get("pattern")
        if pattern is not None and re.search(pattern, value) is None:
            errors.append(f"{path}: string does not match {pattern!r}")

    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if "minimum" in schema and value < schema["minimum"]:
            errors.append(f"{path}: value is below minimum {schema['minimum']}")
    return errors


def _load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def _sha256(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def _report_matching_digest(bundle: "Bundle", digest: str) -> Path | None:
    reports = bundle.directory / "reports"
    if reports.is_symlink() or not reports.is_dir():
        return None
    matches: list[Path] = []
    for path in reports.glob("*.json"):
        try:
            if (
                path.is_symlink()
                or not path.is_file()
                or path.stat().st_size > 4 * 1024 * 1024
                or _sha256(path) != digest
            ):
                continue
        except OSError:
            continue
        matches.append(path)
    return matches[0] if len(matches) == 1 else None


def _safe_repo_path(raw_path: str) -> tuple[Path | None, str | None]:
    posix = PurePosixPath(raw_path)
    if posix.is_absolute() or ".." in posix.parts or "\\" in raw_path:
        return None, "path must be repository-relative and traversal-free"
    resolved = (REPO_ROOT / posix).resolve()
    try:
        resolved.relative_to(REPO_ROOT.resolve())
    except ValueError:
        return None, "path escapes the repository"
    return resolved, None


def _duplicates(values: Iterable[str]) -> set[str]:
    seen: set[str] = set()
    duplicates: set[str] = set()
    for value in values:
        if value in seen:
            duplicates.add(value)
        seen.add(value)
    return duplicates


def _collect_claim_refs(value: Any, path: str = "$") -> list[tuple[str, str]]:
    result: list[tuple[str, str]] = []
    if isinstance(value, dict):
        for key, child in value.items():
            child_path = f"{path}.{key}"
            if key == "claim_refs" and isinstance(child, list):
                result.extend((child_path, item) for item in child if isinstance(item, str))
            else:
                result.extend(_collect_claim_refs(child, child_path))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            result.extend(_collect_claim_refs(child, f"{path}[{index}]"))
    return result


@dataclass(frozen=True)
class Bundle:
    directory: Path
    documents: Mapping[str, Any]

    @property
    def label(self) -> str:
        return str(self.directory.relative_to(REPO_ROOT))

    def document(self, name: str) -> Any:
        return self.documents[name]


def _load_bundle(release_path: Path, schemas: Mapping[str, Mapping[str, Any]]) -> tuple[Bundle | None, list[str]]:
    directory = release_path.parent
    documents: dict[str, Any] = {}
    errors: list[str] = []
    for filename, schema_filename in DOCUMENT_SCHEMAS.items():
        path = directory / filename
        if not path.is_file():
            errors.append(f"{directory.relative_to(REPO_ROOT)}: missing {filename}")
            continue
        try:
            document = _load_json(path)
        except (OSError, json.JSONDecodeError) as error:
            errors.append(f"{path.relative_to(REPO_ROOT)}: invalid JSON: {error}")
            continue
        documents[filename] = document
        schema_errors = validate_json_schema(document, schemas[schema_filename])
        errors.extend(f"{path.relative_to(REPO_ROOT)}: {error}" for error in schema_errors)
    if errors or len(documents) != len(DOCUMENT_SCHEMAS):
        return None, errors
    return Bundle(directory, documents), []


def _validate_evidence(bundle: Bundle) -> tuple[list[str], set[str]]:
    errors: list[str] = []
    evidence = bundle.document("evidence.json")
    claims = evidence["claims"]
    claim_ids = [claim["claim_id"] for claim in claims]
    for duplicate in sorted(_duplicates(claim_ids)):
        errors.append(f"{bundle.label}/evidence.json: duplicate claim {duplicate}")
    claim_set = set(claim_ids)

    referenced_fixtures: set[Path] = set()
    for claim in claims:
        for source in claim["sources"]:
            resolved, path_error = _safe_repo_path(source["path"])
            prefix = f"{bundle.label}/evidence.json claim {claim['claim_id']}"
            if path_error:
                errors.append(f"{prefix}: {source['path']}: {path_error}")
                continue
            assert resolved is not None
            if not resolved.is_file():
                errors.append(f"{prefix}: evidence path does not exist: {source['path']}")
                continue
            if source["sha256"] is not None and source["sha256"] != _sha256(resolved):
                errors.append(f"{prefix}: digest mismatch for {source['path']}")
            if source["kind"] == "sanitized_fixture":
                if source["sha256"] is None:
                    errors.append(f"{prefix}: sanitized fixture must carry a digest")
                referenced_fixtures.add(resolved)

    fixture_root = bundle.directory / "fixtures"
    committed_fixtures = set(fixture_root.rglob("*.json")) if fixture_root.is_dir() else set()
    for fixture in sorted(committed_fixtures):
        findings = scan_fixture_file(fixture)
        errors.extend(f"{fixture.relative_to(REPO_ROOT)}: {finding}" for finding in findings)
        try:
            fixture_value = _load_json(fixture)
        except (OSError, json.JSONDecodeError):
            continue
        metadata = fixture_value.get("_fixture") if isinstance(fixture_value, dict) else None
        if not isinstance(metadata, dict) or metadata.get("sanitizer_version") != SANITIZER_VERSION:
            errors.append(
                f"{fixture.relative_to(REPO_ROOT)}: missing or unsupported RFC 012A "
                f"fixture metadata (expected sanitizer_version {SANITIZER_VERSION})"
            )
        if fixture not in referenced_fixtures:
            errors.append(f"{fixture.relative_to(REPO_ROOT)}: fixture is not referenced by an evidence claim")
    for fixture in sorted(referenced_fixtures - committed_fixtures):
        errors.append(f"{fixture.relative_to(REPO_ROOT)}: fixture evidence is outside its candidate fixture directory")
    if evidence["sanitizer"]["prohibited_scan"] == "pass" and any("fixtures/" in error for error in errors):
        errors.append(f"{bundle.label}/evidence.json: prohibited_scan says pass but fixture scanning failed")
    return errors, claim_set


def _validate_cross_references(bundle: Bundle, claim_set: set[str]) -> list[str]:
    errors: list[str] = []
    referenced: set[str] = set()
    for filename in ("ads.json", "source-declarations.json", "scope-programs.json", "conformance.json", "support-release.json"):
        for path, claim_id in _collect_claim_refs(bundle.document(filename)):
            referenced.add(claim_id)
            if claim_id not in claim_set:
                errors.append(f"{bundle.label}/{filename} {path}: unknown evidence claim {claim_id}")
    for orphan in sorted(claim_set - referenced):
        errors.append(f"{bundle.label}/evidence.json: unreferenced evidence claim {orphan}")
    return errors


def _validate_source_contract(bundle: Bundle) -> list[str]:
    errors: list[str] = []
    ads = bundle.document("ads.json")
    declaration = bundle.document("source-declarations.json")
    roots = {item["root_id"] for item in ads["source_instance"]["canonical_roots"]}
    families = {item["family_id"]: item for item in ads["object_families"]}
    if len(families) != len(ads["object_families"]):
        errors.append(f"{bundle.label}/ads.json: duplicate object family id")

    streams = {item["stream_id"]: item for item in declaration["streams"]}
    if len(streams) != len(declaration["streams"]):
        errors.append(f"{bundle.label}/source-declarations.json: duplicate stream id")

    declared_by_family: dict[str, set[str]] = {}
    ownership: dict[str, str] = {}
    for stream_id, stream in streams.items():
        prefix = f"{bundle.label}/source-declarations.json stream {stream_id}"
        if stream["root_id"] not in roots:
            errors.append(f"{prefix}: unknown root {stream['root_id']}")
        if stream["family_id"] not in families:
            errors.append(f"{prefix}: unknown object family {stream['family_id']}")
        declared_by_family.setdefault(stream["family_id"], set()).add(stream_id)
        for pattern in stream["relative_patterns"]:
            posix = PurePosixPath(pattern)
            if posix.is_absolute() or ".." in posix.parts or "\\" in pattern:
                errors.append(f"{prefix}: unsafe relative pattern {pattern!r}")
            if "**" in pattern and not {"max_depth", "max_entries"}.issubset(stream["bounds"]):
                errors.append(f"{prefix}: recursive pattern requires max_depth and max_entries")
        if stream["primitive"] == "DirectoryMembership" and not {"max_depth", "max_entries"}.issubset(stream["bounds"]):
            errors.append(f"{prefix}: directory membership requires max_depth and max_entries")
        for owned in stream["disposition_ownership"]:
            previous = ownership.get(owned)
            if previous is not None:
                errors.append(f"{prefix}: semantic ownership {owned!r} is already owned by {previous}")
            ownership[owned] = stream_id

    for family_id, family in families.items():
        expected = set(family["stream_ids"])
        actual = declared_by_family.get(family_id, set())
        if expected != actual:
            errors.append(
                f"{bundle.label}: family {family_id} stream set differs between ADS {sorted(expected)} "
                f"and declaration {sorted(actual)}"
            )
    return errors


def _is_scope_source_pattern(value: object) -> bool:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > 4096:
        return False
    first = value.split("/", 1)[0]
    if (
        value.startswith("/")
        or (len(first) >= 2 and first[0].isascii() and first[0].isalpha() and first[1] == ":")
        or "\\" in value
        or "**/**" in value
        or any(ord(character) < 32 or ord(character) == 127 for character in value)
        or any(character in value for character in "?[]{}")
    ):
        return False
    components = value.split("/")
    return all(
        component not in {"", ".", ".."}
        and ("**" not in component or component == "**")
        for component in components
    )


def _scope_locator_pattern(locator: object, identity_inputs: object) -> str | None:
    if not isinstance(locator, str) or not isinstance(identity_inputs, list):
        return None
    declared = set(identity_inputs)
    placeholders: set[str] = set()
    output: list[str] = []
    literal_start = 0
    cursor = 0
    while cursor < len(locator):
        if locator[cursor] == "{":
            output.append(locator[literal_start:cursor])
            end = locator.find("}", cursor + 1)
            if end < 0:
                return None
            name = locator[cursor + 1 : end]
            if (
                "{" in name
                or re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,127}", name) is None
                or name not in declared
                or name in placeholders
            ):
                return None
            placeholders.add(name)
            output.append("*")
            cursor = end + 1
            literal_start = cursor
        elif locator[cursor] == "}":
            return None
        else:
            cursor += 1
    output.append(locator[literal_start:])
    result = "".join(output)
    return result if placeholders and _is_scope_source_pattern(result) else None


def _validate_scope_contract(bundle: Bundle) -> list[str]:
    errors: list[str] = []
    ads = bundle.document("ads.json")
    scope = bundle.document("scope-programs.json")
    source = bundle.document("source-declarations.json")
    source_streams = {item["stream_id"]: item for item in source["streams"]}
    roots = {item["root_id"] for item in ads["source_instance"]["canonical_roots"]}
    if not set(scope["roots"]).issubset(roots):
        errors.append(f"{bundle.label}/scope-programs.json: declares an unknown access root")
    if scope["status"] == "incomplete" and not scope["blockers"]:
        errors.append(f"{bundle.label}/scope-programs.json: incomplete scope requires blockers")
    if scope["status"] in {"candidate", "promoted"} and not scope["programs"]:
        errors.append(f"{bundle.label}/scope-programs.json: candidate/promoted scope requires a program")

    relation_ids: list[str] = []
    observation_primitives = {
        "SiblingObject",
        "ChildDirectoryByNativeId",
        "ReferencedObjectFromField",
        "BoundedIndexLookup",
        "ParameterizedSQLiteRows",
        "KeyNamespace",
    }
    executable_observation_primitives = {
        "SiblingObject",
        "ChildDirectoryByNativeId",
        "ReferencedObjectFromField",
    }
    for program in scope["programs"]:
        root_relation_id = program.get("root_relation_id")
        if scope["status"] == "promoted" and root_relation_id is None:
            errors.append(
                f"{bundle.label}/scope-programs.json program {program['program_id']}: "
                "promoted scope requires a declared root relation"
            )
        for relation in program["relations"]:
            relation_ids.append(relation["relation_id"])
            prefix = f"{bundle.label}/scope-programs.json relation {relation['relation_id']}"
            if relation["access_root"] not in roots:
                errors.append(f"{prefix}: unknown access root {relation['access_root']}")
            locator = relation["locator"]
            posix = PurePosixPath(locator)
            if (
                posix.is_absolute()
                or ".." in posix.parts
                or "\\" in locator
                or "**" in locator
                or any(character in locator for character in ("*", "?", "[", "]"))
            ):
                errors.append(f"{prefix}: locator is not a restricted relative/template locator")
            is_sql = relation["primitive"] == "ParameterizedSQLiteRows"
            if is_sql and ("statement_id" not in relation or not relation.get("parameter_names")):
                errors.append(f"{prefix}: parameterized SQLite relation needs statement_id and parameters")
            if not is_sql and ("statement_id" in relation or "parameter_names" in relation):
                errors.append(f"{prefix}: SQL declaration fields are forbidden for this primitive")
            source_binding = relation.get("source_binding")
            is_artifact = relation["primitive"] == "ArtifactLocatorFromEvidence"
            if scope["status"] == "promoted" and is_artifact and source_binding is None:
                errors.append(f"{prefix}: promoted artifact relation requires a source binding")
            if not is_artifact and source_binding is not None:
                errors.append(f"{prefix}: only an artifact relation may declare a source binding")
            if source_binding is not None:
                if source_binding["max_object_bytes"] > relation["bounds"]["max_bytes"]:
                    errors.append(f"{prefix}: source object bound exceeds the relation byte budget")
                stream = source_streams.get(source_binding["stream_id"])
                if stream is None:
                    errors.append(
                        f"{prefix}: source binding names unknown stream {source_binding['stream_id']}"
                    )
                else:
                    if stream["root_id"] != relation["access_root"]:
                        errors.append(f"{prefix}: source binding root differs from relation root")
                    if stream["primitive"] != source_binding["primitive"]:
                        errors.append(f"{prefix}: source binding primitive differs from source declaration")
                    if stream["bounds"].get("max_object_bytes") != source_binding["max_object_bytes"]:
                        errors.append(f"{prefix}: source binding object bound differs from source declaration")
                    if "scoped" not in stream["topologies"]:
                        errors.append(f"{prefix}: source binding stream does not declare scoped topology")
                    if stream["implementation_state"] != "existing":
                        errors.append(f"{prefix}: source binding stream is not implemented")
                    if stream["safe_decoder_state_boundary"] != "object_generation_revision":
                        errors.append(f"{prefix}: source binding stream lacks generation/revision boundary")
                    required_lifecycle = {"replace", "delete", "recreate"}
                    if not required_lifecycle.issubset(stream["lifecycle"]):
                        errors.append(f"{prefix}: source binding stream lacks replace/delete/recreate lifecycle")
            observation_binding = relation.get("observation_binding")
            is_observation = relation["primitive"] in observation_primitives
            directory_identity_authority = relation.get("directory_identity_authority")
            is_directory = relation["primitive"] == "ChildDirectoryByNativeId"
            if is_directory and directory_identity_authority is None:
                errors.append(
                    f"{prefix}: child-directory relation requires an explicit identity authority"
                )
            elif is_directory and directory_identity_authority not in {
                "configured_root",
                "scope_join",
            }:
                errors.append(
                    f"{prefix}: child-directory identity authority is invalid"
                )
            if not is_directory and directory_identity_authority is not None:
                errors.append(
                    f"{prefix}: only a child-directory relation may declare a directory identity authority"
                )
            if scope["status"] == "promoted" and is_observation and observation_binding is None:
                errors.append(f"{prefix}: promoted dynamic observation relation requires an executable source binding")
            if observation_binding is not None and relation["primitive"] not in executable_observation_primitives:
                errors.append(f"{prefix}: observation source binding is not supported for this relation primitive")
            if observation_binding is not None:
                relative_selector = observation_binding.get("relative_selector")
                locator_pattern = _scope_locator_pattern(
                    locator, relation.get("identity_inputs")
                )
                if locator_pattern is None:
                    errors.append(f"{prefix}: observation locator template is invalid")
                if not _is_scope_source_pattern(observation_binding.get("source_pattern")):
                    errors.append(f"{prefix}: observation source pattern is not canonical")
                if is_directory and relative_selector is None:
                    errors.append(f"{prefix}: child-directory observation binding requires a relative selector")
                if is_directory and relative_selector is not None and not _is_scope_source_pattern(relative_selector):
                    errors.append(f"{prefix}: observation relative selector is not canonical")
                if is_directory and _is_scope_source_pattern(relative_selector):
                    if locator_pattern is None or f"{locator_pattern}/{relative_selector}" != observation_binding.get("source_pattern"):
                        errors.append(f"{prefix}: child-directory selector does not compose to its declared source pattern")
                if (
                    relation["primitive"] == "ReferencedObjectFromField"
                    and locator_pattern != observation_binding.get("source_pattern")
                ):
                    errors.append(f"{prefix}: referenced-object locator does not compose to its declared source pattern")
                if not is_directory and relative_selector is not None:
                    errors.append(f"{prefix}: exact-object observation binding cannot declare a relative selector")
                stream = source_streams.get(observation_binding["stream_id"])
                if stream is None:
                    errors.append(
                        f"{prefix}: observation binding names unknown stream {observation_binding['stream_id']}"
                    )
                else:
                    source_pattern = observation_binding["source_pattern"]
                    if source_pattern not in stream.get("relative_patterns", []):
                        errors.append(f"{prefix}: observation source pattern is not declared by the stream")
                    if stream["root_id"] != relation["access_root"]:
                        errors.append(f"{prefix}: observation binding root differs from relation root")
                    if "scoped" not in stream["topologies"]:
                        errors.append(f"{prefix}: observation binding stream does not declare scoped topology")
                    if stream["implementation_state"] != "existing":
                        errors.append(f"{prefix}: observation binding stream is not implemented")
                    primitive = stream["primitive"]
                    lifecycle = set(stream["lifecycle"])
                    boundary = stream["safe_decoder_state_boundary"]
                    bounds = stream["bounds"]
                    if primitive in {"ReplaceDocument", "PresenceObject"}:
                        if boundary != "object_generation_revision" or not {
                            "replace",
                            "delete",
                            "recreate",
                        }.issubset(lifecycle):
                            errors.append(f"{prefix}: observation object stream lacks a complete revision lifecycle")
                        if bounds.get("max_object_bytes", relation["bounds"]["max_bytes"] + 1) > relation["bounds"]["max_bytes"]:
                            errors.append(f"{prefix}: observation object bound exceeds the relation byte budget")
                    elif primitive == "AppendDelimited":
                        if boundary != "object_generation_cursor" or not {
                            "append",
                            "partial_write",
                            "truncate",
                            "identity_change",
                            "delete",
                            "recreate",
                        }.issubset(lifecycle):
                            errors.append(f"{prefix}: observation append stream lacks a complete cursor lifecycle")
                        if bounds.get("max_record_bytes", relation["bounds"]["max_bytes"] + 1) > relation["bounds"]["max_bytes"]:
                            errors.append(f"{prefix}: observation record bound exceeds the relation byte budget")
                        if bounds.get("max_batch_bytes", relation["bounds"]["max_bytes"] + 1) > relation["bounds"]["max_bytes"]:
                            errors.append(f"{prefix}: observation batch bound exceeds the relation byte budget")
                    else:
                        errors.append(f"{prefix}: observation binding stream primitive is not executable")
        if root_relation_id is not None:
            root_relations = [
                relation
                for relation in program["relations"]
                if relation["relation_id"] == root_relation_id
            ]
            if len(root_relations) != 1:
                errors.append(
                    f"{bundle.label}/scope-programs.json program {program['program_id']}: "
                    f"root relation {root_relation_id} is not declared exactly once"
                )
            elif root_relations[0]["primitive"] != "KnownObject":
                errors.append(
                    f"{bundle.label}/scope-programs.json program {program['program_id']}: "
                    f"root relation {root_relation_id} must use KnownObject"
                )
    for duplicate in sorted(_duplicates(relation_ids)):
        errors.append(f"{bundle.label}/scope-programs.json: duplicate relation id {duplicate}")
    return errors


def _validate_release(bundle: Bundle) -> list[str]:
    errors: list[str] = []
    release = bundle.document("support-release.json")
    ads = bundle.document("ads.json")
    source = bundle.document("source-declarations.json")
    scope = bundle.document("scope-programs.json")
    evidence = bundle.document("evidence.json")
    conformance = bundle.document("conformance.json")

    adapter_ids = {
        release["adapter_id"],
        ads["adapter_id"],
        source["adapter_id"],
        scope["adapter_id"],
        evidence["adapter_id"],
        conformance["adapter_id"],
    }
    if len(adapter_ids) != 1:
        errors.append(f"{bundle.label}: adapter IDs differ across the bundle")
    ads_ids = {ads["ads_id"], source["ads_id"], scope["ads_id"], evidence["ads_id"]}
    if len(ads_ids) != 1:
        errors.append(f"{bundle.label}: ADS IDs differ across the bundle")
    if conformance["support_release_id"] != release["support_release_id"]:
        errors.append(f"{bundle.label}: conformance support-release ID does not match ledger")
    if release["artifact_compatibility"]["family"] != ads["native_artifact"]["family"]:
        errors.append(f"{bundle.label}: artifact family differs between ADS and ledger")
    if ads["scope_program_manifest"] != str((bundle.directory / "scope-programs.json").relative_to(REPO_ROOT)):
        errors.append(f"{bundle.label}/ads.json: scope_program_manifest does not name this bundle")

    expected_reference_files = {
        "ads": "ads.json",
        "source_declaration": "source-declarations.json",
        "scope_program": "scope-programs.json",
        "evidence": "evidence.json",
        "conformance": "conformance.json",
    }
    for reference_name, filename in expected_reference_files.items():
        reference = release["references"][reference_name]
        expected_path = str((bundle.directory / filename).relative_to(REPO_ROOT))
        if reference["path"] != expected_path:
            errors.append(f"{bundle.label}/support-release.json: {reference_name} path must be {expected_path}")
        if reference["sha256"] != _sha256(bundle.directory / filename):
            errors.append(f"{bundle.label}/support-release.json: {reference_name} digest mismatch")

    source_streams = {item["stream_id"]: item for item in source["streams"]}
    release_streams = {item["stream_id"]: item for item in release["stream_contracts"]}
    if set(source_streams) != set(release_streams):
        errors.append(f"{bundle.label}: stream-contract set differs between source declaration and ledger")
    for stream_id in set(source_streams) & set(release_streams):
        for field_name in ("overlap_strategy", "safe_decoder_state_boundary"):
            if source_streams[stream_id][field_name] != release_streams[stream_id][field_name]:
                errors.append(f"{bundle.label}: {stream_id} {field_name} differs between declaration and ledger")

    check_ids = [item["check_id"] for item in conformance["checks"]]
    missing_checks = REQUIRED_CONFORMANCE_CHECKS - set(check_ids)
    for check_id in sorted(missing_checks):
        errors.append(f"{bundle.label}/conformance.json: missing required check {check_id}")
    for duplicate in sorted(_duplicates(check_ids)):
        errors.append(f"{bundle.label}/conformance.json: duplicate check {duplicate}")

    # A release is identified by its dated version, which is its directory. The
    # maturity tier lives in one place — the scope program's declared status —
    # so a release cannot claim one thing and its declarations another.
    if bundle.directory.name != release["version"]:
        errors.append(
            f"{bundle.label}: directory name does not match release version {release['version']}"
        )
    promoted = scope["status"] == "promoted"
    if not promoted:
        if not release["promotion_blockers"]:
            errors.append(
                f"{bundle.label}: an unpromoted support release must name promotion blockers"
            )
        if release["lifecycle"]["promoted_at"] is not None:
            errors.append(f"{bundle.label}: an unpromoted release cannot have promoted_at")
    else:
        if release["promotion_blockers"]:
            errors.append(f"{bundle.label}: promoted support release cannot have blockers")
        if release["sanitizer_review"]["status"] != "approved":
            errors.append(f"{bundle.label}: promoted support release needs approved sanitizer review")
        if evidence["sanitizer"]["prohibited_scan"] != "pass":
            errors.append(f"{bundle.label}: promoted support release needs a passing prohibited scan")
        if release["reports"]["conformance_sha256"] is None or release["reports"]["performance_sha256"] is None:
            errors.append(f"{bundle.label}: promoted support release needs conformance and performance reports")
        else:
            conformance_report = _report_matching_digest(
                bundle, release["reports"]["conformance_sha256"]
            )
            performance_report = _report_matching_digest(
                bundle, release["reports"]["performance_sha256"]
            )
            if conformance_report is None:
                errors.append(
                    f"{bundle.label}: conformance report digest does not uniquely bind a retained report"
                )
            elif release["adapter_id"] != "fixture-agent":
                try:
                    report = _load_json(conformance_report)
                except (OSError, json.JSONDecodeError):
                    report = None
                if (
                    not isinstance(report, dict)
                    or report.get("adapter_id") != release["adapter_id"]
                    or report.get("support_release_id") != release["support_release_id"]
                    or not isinstance(report.get("checks"), list)
                    or not report["checks"]
                ):
                    errors.append(
                        f"{bundle.label}: promoted conformance report does not bind the release and retained checks"
                    )
            if performance_report is None:
                errors.append(
                    f"{bundle.label}: performance report digest does not uniquely bind a retained report"
                )
            elif release["adapter_id"] != "fixture-agent":
                try:
                    report = _load_json(performance_report)
                except (OSError, json.JSONDecodeError):
                    report = None
                required_performance_fields = {
                    "support_release_id",
                    "source_fixture_digests",
                    "environment",
                    "cache_method",
                    "repetitions",
                    "measurements",
                    "statistics",
                    "semantic_digests",
                    "coverage",
                    "observer",
                    "usage",
                    "timestamps",
                    "query_distributions",
                    "resources",
                    "contract_versions",
                }
                if not isinstance(report, dict) or not required_performance_fields.issubset(report):
                    errors.append(
                        f"{bundle.label}: promoted performance report does not satisfy the RFC 012 benchmark-report shape"
                    )
                elif report["support_release_id"] != release["support_release_id"]:
                    errors.append(
                        f"{bundle.label}: promoted performance report names a different support release"
                    )
                elif not isinstance(report["repetitions"], int) or isinstance(
                    report["repetitions"], bool
                ) or report["repetitions"] < 3:
                    errors.append(
                        f"{bundle.label}: promoted performance report needs at least three repetitions"
                    )
        reviewer = release["sanitizer_review"]["reviewer"]
        reviewed_at = release["sanitizer_review"]["reviewed_at"]
        if release["adapter_id"] != "fixture-agent" and (
            not isinstance(reviewer, str)
            or not reviewer.strip()
            or reviewer.strip().lower()
            in {"rfc012-integrator", "automation", "unknown", "pending"}
            or not isinstance(reviewed_at, str)
            or not reviewed_at.strip()
        ):
            errors.append(
                f"{bundle.label}: promoted support release needs a named independent sanitizer reviewer and date"
            )
        if any(item["status"] not in {"pass", "not_applicable"} for item in conformance["checks"]):
            errors.append(f"{bundle.label}: promoted support release has unfinished conformance checks")
        if any(claim["state"] in {"open", "degraded"} for claim in evidence["claims"]):
            errors.append(f"{bundle.label}: promoted support release has open/degraded evidence claims")
        if any(item["state"] == "open" for item in release["drift_signatures"]):
            errors.append(f"{bundle.label}: promoted support release has open drift")
        if not (
            release["artifact_compatibility"]["exact_versions"]
            or release["artifact_compatibility"]["ranges"]
        ):
            errors.append(f"{bundle.label}: promoted support release needs exact or ranged artifact coverage")
        if any(document["status"] != "promoted" for document in (ads, source, scope)):
            errors.append(f"{bundle.label}: promoted ledger requires promoted ADS/source/scope documents")
        if release["lifecycle"]["promoted_at"] is None:
            errors.append(f"{bundle.label}: promoted support release needs promoted_at")
    # Retirement is expressed by removing the bundle, not by a status a
    # compiled adapter could still bind to.
    return errors


def validate_bundle(bundle: Bundle) -> list[str]:
    errors, claim_set = _validate_evidence(bundle)
    errors.extend(_validate_cross_references(bundle, claim_set))
    errors.extend(_validate_source_contract(bundle))
    errors.extend(_validate_scope_contract(bundle))
    errors.extend(_validate_release(bundle))
    errors.extend(f"{bundle.label}: {item}" for item in validate_numeric_ranges([bundle.document("support-release.json")]))
    return errors


def validate_repository() -> tuple[list[Bundle], list[str]]:
    errors: list[str] = []
    schemas: dict[str, Mapping[str, Any]] = {}
    for filename in DOCUMENT_SCHEMAS.values():
        path = SCHEMA_ROOT / filename
        try:
            schema = _load_json(path)
        except (OSError, json.JSONDecodeError) as error:
            errors.append(f"{path.relative_to(REPO_ROOT)}: invalid schema JSON: {error}")
            continue
        if not isinstance(schema, dict) or "$id" not in schema:
            errors.append(f"{path.relative_to(REPO_ROOT)}: schema must be an object with $id")
            continue
        schemas[filename] = schema
    if len(schemas) != len(DOCUMENT_SCHEMAS):
        return [], errors

    release_paths = sorted(SUPPORT_ROOT.glob("*/*/support-release.json"))
    if not release_paths:
        errors.append("agent-support: no support releases found")
        return [], errors
    bundles: list[Bundle] = []
    release_ids: list[str] = []
    for release_path in release_paths:
        bundle, load_errors = _load_bundle(release_path, schemas)
        errors.extend(load_errors)
        if bundle is None:
            continue
        bundles.append(bundle)
        release_ids.append(bundle.document("support-release.json")["support_release_id"])
        errors.extend(validate_bundle(bundle))
    for duplicate in sorted(_duplicates(release_ids)):
        errors.append(f"agent-support: duplicate support release ID {duplicate}")
    return bundles, errors


def main() -> int:
    bundles, errors = validate_repository()
    if errors:
        for error in errors:
            print(f"ERROR: {error}")
        print(f"RFC 012A support contracts: {len(errors)} error(s), {len(bundles)} loaded bundle(s)")
        return 1
    promoted = [
        bundle
        for bundle in bundles
        if bundle.document("scope-programs.json")["status"] == "promoted"
    ]
    names = ", ".join(
        f"{bundle.document('support-release.json')['adapter_id']}"
        f"@{bundle.document('support-release.json')['version']}"
        for bundle in promoted
    )
    print(
        "RFC 012A support contracts: "
        f"{len(bundles)} release bundle(s) valid, "
        f"{len(promoted)} selectable for runtime decoding"
        + (f" ({names})" if names else "")
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
