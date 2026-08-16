"""Executable RFC 012A support, negotiation, and access-bound semantics."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Iterable, Mapping, Sequence


class CompatibilityClass(str, Enum):
    EXACT_SUPPORTED = "ExactSupported"
    RANGE_SUPPORTED = "RangeSupported"
    RECOGNIZED_UNVERIFIED = "RecognizedUnverified"
    UNKNOWN_OR_INCOMPATIBLE = "UnknownOrIncompatible"


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
    compatibility_class: CompatibilityClass
    support_release_id: str | None
    reason: str
    permissions: Mapping[str, bool]


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

    entries = [entry for entry in releases if entry["artifact_compatibility"]["family"] == probe.family]
    if not entries:
        selected = CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE
        return CompatibilityResult(selected, None, "unrecognized artifact family", _PERMISSIONS[selected])
    if probe.contradictory_markers:
        selected = CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE
        return CompatibilityResult(selected, None, "contradictory native markers", _PERMISSIONS[selected])

    family_platforms = {
        platform
        for entry in entries
        for platform in entry["artifact_compatibility"]["platforms"]
    }
    if probe.platform not in family_platforms:
        selected = CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE
        return CompatibilityResult(selected, None, "platform is not declared for this family", _PERMISSIONS[selected])

    promoted = [entry for entry in entries if entry["status"] == "promoted"]
    marker_mismatch = False
    for entry in sorted(promoted, key=lambda value: value["support_release_id"], reverse=True):
        compatibility = entry["artifact_compatibility"]
        if probe.platform not in compatibility["platforms"]:
            continue
        required_markers = set(compatibility["required_markers"])
        if not required_markers.issubset(probe.markers):
            marker_mismatch = True
            continue
        if probe.version is None:
            continue
        if probe.version in compatibility["exact_versions"]:
            selected = CompatibilityClass.EXACT_SUPPORTED
            return CompatibilityResult(
                selected,
                entry["support_release_id"],
                "exact promoted artifact version and markers",
                _PERMISSIONS[selected],
            )
        if any(_inside_range(probe.version, item) for item in compatibility["ranges"]):
            selected = CompatibilityClass.RANGE_SUPPORTED
            return CompatibilityResult(
                selected,
                entry["support_release_id"],
                "fixture-backed promoted compatibility range",
                _PERMISSIONS[selected],
            )

    if marker_mismatch and promoted:
        selected = CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE
        return CompatibilityResult(selected, None, "required native marker is absent", _PERMISSIONS[selected])
    selected = CompatibilityClass.RECOGNIZED_UNVERIFIED
    permissions = dict(_PERMISSIONS[selected])
    forward_catalog_release = next(
        (
            entry
            for entry in promoted
            if probe.platform in entry["artifact_compatibility"]["platforms"]
            and entry["artifact_compatibility"]["forward_catalog_only"]
            and set(entry["artifact_compatibility"]["required_markers"]).issubset(probe.markers)
        ),
        None,
    )
    if forward_catalog_release is not None:
        permissions["catalog"] = True
    return CompatibilityResult(
        selected,
        forward_catalog_release["support_release_id"] if forward_catalog_release is not None else None,
        (
            "recognized artifact is limited to a promoted forward-compatible catalog path"
            if forward_catalog_release is not None
            else "recognized artifact has no matching promoted support release"
        ),
        permissions,
    )


class ContractSelectionError(ValueError):
    """Raised before source access when public semantic versions are incompatible."""


_BASE_VERSION_FIELDS = (
    "external_entity_reference_version",
    "semantic_revision_reference_version",
)


def select_contract_versions(
    requested: Mapping[str, Any], offered: Mapping[str, Any]
) -> dict[str, Any]:
    """Select an explicit compatible public contract set.

    Requested scalar versions are mandatory. Version-list fields are ordered
    consumer preferences. No silent downgrade or unknown-family drop occurs.
    """

    if requested.get("model_major") != offered.get("model_major"):
        raise ContractSelectionError("incompatible base model major")
    selection: dict[str, Any] = {"model_major": requested["model_major"]}

    for field_name in _BASE_VERSION_FIELDS:
        requested_version = requested.get(field_name)
        offered_versions = offered.get(field_name, [])
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
            continue
        offered_versions = set(offered.get(field_name, []))
        selected = next((version for version in requested_versions if version in offered_versions), None)
        if selected is None:
            raise ContractSelectionError(f"no compatible {field_name}")
        selection[field_name.removesuffix("s")] = selected

    requested_families = requested.get("fact_family_versions", {})
    offered_families = offered.get("fact_family_versions", {})
    selected_families: dict[str, int] = {}
    for family, preferences in requested_families.items():
        if family not in offered_families:
            raise ContractSelectionError(f"required fact family is absent: {family}")
        offered_versions = set(offered_families[family])
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
