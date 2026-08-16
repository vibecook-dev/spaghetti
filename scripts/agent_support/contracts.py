"""Executable RFC 012A support, negotiation, and access-bound semantics."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Iterable, Mapping, Sequence


SUPPORT_SELECTION_CONTRACT_VERSION = 1
CONTRACT_VERSION_SELECTION_VERSION = 1


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
