"""Executable RFC 012A support, negotiation, and access-bound semantics."""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Iterable, Mapping, Sequence


SUPPORT_SELECTION_CONTRACT_VERSION = 1
CONTRACT_VERSION_SELECTION_VERSION = 1
SCOPE_ACCESS_REPORT_CONTRACT_VERSION = 1


class CompatibilityClass(str, Enum):
    EXACT_SUPPORTED = "ExactSupported"
    RANGE_SUPPORTED = "RangeSupported"
    RECOGNIZED_UNVERIFIED = "RecognizedUnverified"
    UNKNOWN_OR_INCOMPATIBLE = "UnknownOrIncompatible"


class CompatibilityReason(str, Enum):
    EXACT_PROMOTED_VERSION = "exact_promoted_version"
    FIXTURE_BACKED_RANGE = "fixture_backed_range"
    PROMOTED_FORWARD_CATALOG_ONLY = "promoted_forward_catalog_only"
    NO_MATCHING_PROMOTED_RELEASE = "no_matching_promoted_release"
    REQUIRED_NATIVE_MARKER_ABSENT = "required_native_marker_absent"
    PLATFORM_NOT_DECLARED = "platform_not_declared"
    UNRECOGNIZED_ARTIFACT_FAMILY = "unrecognized_artifact_family"
    CONTRADICTORY_NATIVE_MARKERS = "contradictory_native_markers"
    AMBIGUOUS_PROMOTED_RELEASE = "ambiguous_promoted_release"


_PERMISSIONS: dict[CompatibilityClass, dict[str, bool]] = {
    CompatibilityClass.EXACT_SUPPORTED: {
        "version_probe": True,
        "catalog": True,
        "durable": True,
        "scoped_observation": True,
        "bounded_drift": True,
    },
    CompatibilityClass.RANGE_SUPPORTED: {
        "version_probe": True,
        "catalog": True,
        "durable": True,
        "scoped_observation": True,
        "bounded_drift": True,
    },
    CompatibilityClass.RECOGNIZED_UNVERIFIED: {
        "version_probe": True,
        "catalog": False,
        "durable": False,
        "scoped_observation": False,
        "bounded_drift": True,
    },
    CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE: {
        "version_probe": True,
        "catalog": False,
        "durable": False,
        "scoped_observation": False,
        "bounded_drift": False,
    },
}


@dataclass(frozen=True)
class RuntimeProbe:
    family: str
    platform: str
    version: str | None
    markers: frozenset[str] = frozenset()
    contradictory_markers: bool = False


@dataclass(frozen=True)
class CompatibilityResult:
    support_selection_contract_version: int
    compatibility_class: CompatibilityClass
    support_release_id: str | None
    reason: CompatibilityReason
    permissions: Mapping[str, bool]


class SupportContractError(ValueError):
    """A support declaration is invalid or cannot produce an unambiguous decision."""


def _result(
    compatibility_class: CompatibilityClass,
    support_release_id: str | None,
    reason: CompatibilityReason,
    *,
    catalog_override: bool = False,
) -> CompatibilityResult:
    permissions = dict(_PERMISSIONS[compatibility_class])
    if catalog_override:
        permissions["catalog"] = True
    return CompatibilityResult(
        SUPPORT_SELECTION_CONTRACT_VERSION,
        compatibility_class,
        support_release_id,
        reason,
        permissions,
    )


def _version_tuple(value: str) -> tuple[int, ...]:
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)*", value):
        raise ValueError(f"version is not a dotted numeric version: {value!r}")
    return tuple(int(part) for part in value.split("."))


def _pad_versions(left: tuple[int, ...], right: tuple[int, ...]) -> tuple[tuple[int, ...], tuple[int, ...]]:
    size = max(len(left), len(right))
    return left + (0,) * (size - len(left)), right + (0,) * (size - len(right))


def _compare_version(left: str, right: str) -> int:
    left_parts, right_parts = _pad_versions(_version_tuple(left), _version_tuple(right))
    return (left_parts > right_parts) - (left_parts < right_parts)


def _inside_range(version: str, version_range: Mapping[str, Any]) -> bool:
    lower = _compare_version(version, str(version_range["minimum"]))
    upper = _compare_version(version, str(version_range["maximum"]))
    lower_ok = lower >= 0 if version_range["minimum_inclusive"] else lower > 0
    upper_ok = upper <= 0 if version_range["maximum_inclusive"] else upper < 0
    return lower_ok and upper_ok


def classify_runtime(probe: RuntimeProbe, releases: Iterable[Mapping[str, Any]]) -> CompatibilityResult:
    """Classify one native artifact without allowing candidates to confer support."""

    release_list = list(releases)
    release_ids = [str(entry["support_release_id"]) for entry in release_list]
    if len(release_ids) != len(set(release_ids)):
        raise SupportContractError("duplicate support release id")
    numeric_range_errors = validate_numeric_ranges(release_list)
    if numeric_range_errors:
        raise SupportContractError(numeric_range_errors[0])

    entries = [
        entry
        for entry in release_list
        if entry["artifact_compatibility"]["family"] == probe.family
    ]
    if not entries:
        return _result(
            CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE,
            None,
            CompatibilityReason.UNRECOGNIZED_ARTIFACT_FAMILY,
        )
    if probe.contradictory_markers:
        return _result(
            CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE,
            None,
            CompatibilityReason.CONTRADICTORY_NATIVE_MARKERS,
        )

    family_platforms = {
        platform
        for entry in entries
        for platform in entry["artifact_compatibility"]["platforms"]
    }
    if probe.platform not in family_platforms:
        return _result(
            CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE,
            None,
            CompatibilityReason.PLATFORM_NOT_DECLARED,
        )

    promoted_on_platform = [
        entry
        for entry in entries
        if entry["status"] == "promoted"
        and probe.platform in entry["artifact_compatibility"]["platforms"]
    ]
    marker_compatible = []
    for entry in promoted_on_platform:
        compatibility = entry["artifact_compatibility"]
        required_markers = set(compatibility["required_markers"])
        if required_markers.issubset(probe.markers):
            marker_compatible.append(entry)

    matches: list[tuple[Mapping[str, Any], CompatibilityClass]] = []
    if probe.version is not None:
        for entry in marker_compatible:
            compatibility = entry["artifact_compatibility"]
            if probe.version in compatibility["exact_versions"]:
                matches.append((entry, CompatibilityClass.EXACT_SUPPORTED))
                continue
            try:
                inside_declared_range = any(
                    _inside_range(probe.version, item)
                    for item in compatibility["ranges"]
                )
            except ValueError:
                inside_declared_range = False
            if inside_declared_range:
                matches.append((entry, CompatibilityClass.RANGE_SUPPORTED))

    if len(matches) > 1:
        return _result(
            CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE,
            None,
            CompatibilityReason.AMBIGUOUS_PROMOTED_RELEASE,
        )
    if matches:
        entry, selected = matches[0]
        reason = (
            CompatibilityReason.EXACT_PROMOTED_VERSION
            if selected is CompatibilityClass.EXACT_SUPPORTED
            else CompatibilityReason.FIXTURE_BACKED_RANGE
        )
        return _result(selected, str(entry["support_release_id"]), reason)

    if promoted_on_platform and not marker_compatible:
        return _result(
            CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE,
            None,
            CompatibilityReason.REQUIRED_NATIVE_MARKER_ABSENT,
        )

    forward_catalog = [
        entry
        for entry in marker_compatible
        if entry["artifact_compatibility"]["forward_catalog_only"]
    ]
    if len(forward_catalog) > 1:
        return _result(
            CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE,
            None,
            CompatibilityReason.AMBIGUOUS_PROMOTED_RELEASE,
        )
    if forward_catalog:
        return _result(
            CompatibilityClass.RECOGNIZED_UNVERIFIED,
            str(forward_catalog[0]["support_release_id"]),
            CompatibilityReason.PROMOTED_FORWARD_CATALOG_ONLY,
            catalog_override=True,
        )
    return _result(
        CompatibilityClass.RECOGNIZED_UNVERIFIED,
        None,
        CompatibilityReason.NO_MATCHING_PROMOTED_RELEASE,
    )


class ContractSelectionError(ValueError):
    """Raised before source access when public semantic versions are incompatible."""


def _validated_versions(value: Any, label: str, *, require_nonempty: bool) -> list[int]:
    if not isinstance(value, list) or (require_nonempty and not value):
        raise ContractSelectionError(f"{label} must be a version list")
    if any(not isinstance(version, int) or isinstance(version, bool) or version <= 0 for version in value):
        raise ContractSelectionError(f"{label} contains an invalid version")
    if len(value) != len(set(value)):
        raise ContractSelectionError(f"{label} contains duplicate versions")
    return value


def select_contract_versions(
    requested: Mapping[str, Any], offered: Mapping[str, Any]
) -> dict[str, Any]:
    """Select an explicit compatible public contract set.

    Requested scalar versions are mandatory. Version-list fields are ordered
    consumer preferences. No silent downgrade or unknown-family drop occurs.
    """

    if requested.get("selection_contract_version") != CONTRACT_VERSION_SELECTION_VERSION:
        raise ContractSelectionError("unsupported contract-version selection request version")
    if offered.get("selection_contract_version") != CONTRACT_VERSION_SELECTION_VERSION:
        raise ContractSelectionError("unsupported contract-version offer version")
    if requested.get("model_major") != offered.get("model_major"):
        raise ContractSelectionError("incompatible base model major")
    if not isinstance(requested.get("model_major"), int) or requested["model_major"] <= 0:
        raise ContractSelectionError("model major must be greater than zero")
    selection: dict[str, Any] = {
        "selection_contract_version": CONTRACT_VERSION_SELECTION_VERSION,
        "model_major": requested["model_major"],
    }

    for field_name, offer_field_name in (
        ("external_entity_reference_version", "external_entity_reference_versions"),
        ("semantic_revision_reference_version", "semantic_revision_reference_versions"),
    ):
        requested_version = requested.get(field_name)
        if not isinstance(requested_version, int) or isinstance(requested_version, bool) or requested_version <= 0:
            raise ContractSelectionError(f"invalid {field_name}: {requested_version}")
        offered_versions = _validated_versions(
            offered.get(offer_field_name), offer_field_name, require_nonempty=True
        )
        if requested_version not in offered_versions:
            raise ContractSelectionError(f"unsupported {field_name}: {requested_version}")
        selection[field_name] = requested_version

    for field_name in (
        "coverage_contract_versions",
        "query_pack_versions",
        "observation_contract_versions",
    ):
        requested_versions = requested.get(field_name)
        if requested_versions is None:
            if field_name == "coverage_contract_versions":
                raise ContractSelectionError("coverage_contract_versions is required")
            selection[field_name.removesuffix("s")] = None
            continue
        requested_versions = _validated_versions(
            requested_versions, f"requested {field_name}", require_nonempty=True
        )
        offered_versions = set(
            _validated_versions(
                offered.get(field_name),
                f"offered {field_name}",
                require_nonempty=field_name == "coverage_contract_versions",
            )
        )
        selected = next((version for version in requested_versions if version in offered_versions), None)
        if selected is None:
            raise ContractSelectionError(f"no compatible {field_name}")
        selection[field_name.removesuffix("s")] = selected

    requested_families = requested.get("fact_family_versions", {})
    offered_families = offered.get("fact_family_versions", {})
    if not isinstance(requested_families, Mapping) or not isinstance(offered_families, Mapping):
        raise ContractSelectionError("fact_family_versions must be an object")
    selected_families: dict[str, int] = {}
    for family, preferences in requested_families.items():
        if family not in offered_families:
            raise ContractSelectionError(f"required fact family is absent: {family}")
        preferences = _validated_versions(
            preferences, f"requested fact family {family}", require_nonempty=True
        )
        offered_versions = set(
            _validated_versions(
                offered_families[family],
                f"offered fact family {family}",
                require_nonempty=True,
            )
        )
        selected = next((version for version in preferences if version in offered_versions), None)
        if selected is None:
            raise ContractSelectionError(f"no compatible fact-family version: {family}")
        selected_families[family] = selected
    selection["fact_family_versions"] = selected_families
    return selection


class AccessBoundExceeded(RuntimeError):
    """A declared scope relation tried to exceed its common-engine budget."""


class AccessReportError(ValueError):
    """A scope access report cannot be encoded by the v1 canonical contract."""


_ACCESS_OPERATION_CODES = {
    "object_read": 1,
    "parameterized_query": 2,
    "object_listing": 3,
}
_ACCESS_PHASE_CODES = {"initial": 1, "revalidation": 2}
_ACCESS_OUTCOME_CODES = {
    "available": 1,
    "unavailable": 2,
    "oversized": 3,
    "failed": 4,
    "abandoned": 5,
    "denied": 6,
}
_ACCESS_LIMIT_CODES = {
    "max_fan_out": 1,
    "max_depth": 2,
    "max_objects": 3,
    "max_bytes": 4,
    "max_rows": 5,
    "reservation": 6,
}


def scope_access_report_digest(report: Mapping[str, Any]) -> str:
    """Return the RFC 012A v1 canonical SHA-256 integrity digest."""

    digest = hashlib.sha256()
    digest.update(b"spaghetti/rfc012a/scope-access-report/v1\0")
    _report_u32(digest, report["scope_access_report_contract_version"])
    _report_component(digest, _report_text(report["adapter_id"]))
    _report_component(digest, _report_text(report["support_release_id"]))
    _report_component(digest, _report_digest_bytes(report["support_release_digest"]))
    _report_component(digest, _report_digest_bytes(report["scope_program_digest"]))
    _report_component(digest, _report_text(report["declaration_id"]))
    _report_component(digest, _report_text(report["program_id"]))
    _report_u32(digest, report["selection_contract_version"])
    _report_u32(digest, report["observation_contract_version"])
    relations = report["relations"]
    _report_u64(digest, len(relations))
    for relation in relations:
        _report_relation(digest, relation)
    return f"sha256:{digest.hexdigest()}"


def verify_scope_access_report_digest(report: Mapping[str, Any]) -> bool:
    expected = _report_digest_bytes(report["digest"])
    return scope_access_report_digest(report) == f"sha256:{expected.hex()}"


def _report_relation(digest: Any, relation: Mapping[str, Any]) -> None:
    _report_u32(digest, relation["access_trace_contract_version"])
    _report_component(digest, _report_text(relation["relation_id"]))
    bounds = relation["bounds"]
    _report_u64(digest, bounds["max_fan_out"])
    _report_u32(digest, bounds["max_depth"])
    _report_u64(digest, bounds["max_objects"])
    _report_u64(digest, bounds["max_bytes"])
    _report_u64(digest, bounds["max_rows"])
    for field_name in (
        "attempts",
        "reservations_granted",
        "completed",
        "denied",
        "abandoned",
        "objects_accessed",
        "bytes_read",
        "rows_read",
    ):
        _report_u64(digest, relation[field_name])
    _report_u32(digest, relation["max_depth_observed"])
    for field_name in ("bytes_reserved", "rows_reserved", "trace_entries_dropped"):
        _report_u64(digest, relation[field_name])
    trace = relation["trace"]
    _report_u64(digest, len(trace))
    for entry in trace:
        _report_trace_entry(digest, entry)


def _report_trace_entry(digest: Any, entry: Mapping[str, Any]) -> None:
    _report_u32(digest, entry["access_trace_contract_version"])
    _report_u64(digest, entry["sequence"])
    _report_component(digest, _report_text(entry["relation_id"]))
    _report_code(digest, _ACCESS_OPERATION_CODES, entry["operation"], "operation")
    _report_code(digest, _ACCESS_PHASE_CODES, entry["phase"], "phase")
    parent = entry["parent_token"]
    if parent is None:
        digest.update(b"\x00")
    else:
        digest.update(b"\x01")
        _report_component(digest, _report_digest_bytes(parent))
    _report_component(digest, _report_digest_bytes(entry["object_token"]))
    _report_u32(digest, entry["depth"])
    for field_name in ("reserved_bytes", "reserved_rows", "bytes_read", "rows_read"):
        _report_u64(digest, entry[field_name])
    _report_code(digest, _ACCESS_OUTCOME_CODES, entry["outcome"], "outcome")
    denied_limit = entry["denied_limit"]
    if denied_limit is None:
        digest.update(b"\x00")
    else:
        digest.update(b"\x01")
        _report_code(digest, _ACCESS_LIMIT_CODES, denied_limit, "denied limit")


def _report_code(digest: Any, codes: Mapping[str, int], value: Any, label: str) -> None:
    if value not in codes:
        raise AccessReportError(f"unknown access-report {label}: {value!r}")
    digest.update(bytes((codes[value],)))


def _report_text(value: Any) -> bytes:
    if not isinstance(value, str):
        raise AccessReportError("access-report text value must be a string")
    return value.encode("utf-8")


def _report_digest_bytes(value: Any) -> bytes:
    if (
        not isinstance(value, list)
        or len(value) != 32
        or any(not isinstance(item, int) or isinstance(item, bool) or not 0 <= item <= 255 for item in value)
    ):
        raise AccessReportError("access-report digest/token must contain 32 bytes")
    return bytes(value)


def _report_component(digest: Any, value: bytes) -> None:
    _report_u64(digest, len(value))
    digest.update(value)


def _report_u32(digest: Any, value: Any) -> None:
    _report_unsigned(digest, value, 4)


def _report_u64(digest: Any, value: Any) -> None:
    _report_unsigned(digest, value, 8)


def _report_unsigned(digest: Any, value: Any, width: int) -> None:
    if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value < 1 << (width * 8):
        raise AccessReportError(f"access-report value does not fit u{width * 8}")
    digest.update(value.to_bytes(width, "big"))


@dataclass(frozen=True)
class AccessRecord:
    relation_id: str
    object_token: str
    bytes_read: int
    rows_read: int
    depth: int


@dataclass
class AccessBudget:
    """Deterministic bound accounting used by scope conformance tooling."""

    relation_id: str
    max_fan_out: int
    max_depth: int
    max_objects: int
    max_bytes: int
    max_rows: int
    records: list[AccessRecord] = field(default_factory=list)
    _object_tokens: set[str] = field(default_factory=set, init=False, repr=False)

    def consume(self, object_token: str, *, bytes_read: int, rows_read: int = 0, depth: int = 1) -> None:
        if bytes_read < 0 or rows_read < 0 or depth < 1:
            raise ValueError("access accounting values must be non-negative and depth must be positive")
        next_objects = set(self._object_tokens)
        next_objects.add(object_token)
        next_bytes = sum(record.bytes_read for record in self.records) + bytes_read
        next_rows = sum(record.rows_read for record in self.records) + rows_read
        limits = (
            (len(next_objects) > self.max_objects, "max_objects"),
            (len(next_objects) > self.max_fan_out, "max_fan_out"),
            (depth > self.max_depth, "max_depth"),
            (next_bytes > self.max_bytes, "max_bytes"),
            (next_rows > self.max_rows, "max_rows"),
        )
        for exceeded, name in limits:
            if exceeded:
                raise AccessBoundExceeded(f"{self.relation_id} exceeded {name}")
        self._object_tokens = next_objects
        self.records.append(AccessRecord(self.relation_id, object_token, bytes_read, rows_read, depth))

    @property
    def totals(self) -> Mapping[str, int]:
        return {
            "objects": len(self._object_tokens),
            "bytes": sum(record.bytes_read for record in self.records),
            "rows": sum(record.rows_read for record in self.records),
            "max_depth": max((record.depth for record in self.records), default=0),
        }


def validate_numeric_ranges(releases: Sequence[Mapping[str, Any]]) -> list[str]:
    errors: list[str] = []
    for release in releases:
        for index, version_range in enumerate(release["artifact_compatibility"]["ranges"]):
            prefix = f"{release['support_release_id']}.artifact_compatibility.ranges[{index}]"
            try:
                order = _compare_version(version_range["minimum"], version_range["maximum"])
            except ValueError as error:
                errors.append(f"{prefix}: {error}")
                continue
            if order > 0:
                errors.append(f"{prefix}: minimum exceeds maximum")
            if order == 0 and not (
                version_range["minimum_inclusive"] and version_range["maximum_inclusive"]
            ):
                errors.append(f"{prefix}: empty range")
    return errors
