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
ACCESS_REQUEST_CONTRACT_VERSION = 1
ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION = 1
_MAX_ACCESS_REQUEST_GRANTS = 256
_MAX_ACCESS_REQUEST_IDENTITY_INPUTS = 32
_MAX_ACCESS_REQUEST_MARKERS = 64
_MAX_ACCESS_REQUEST_FACT_FAMILIES = 64
_MAX_ACCESS_REQUEST_ENCODED_BYTES = 64 * 1024
_MAX_ACCESS_REQUEST_IDENTIFIER_BYTES = 128
_REQUEST_MACHINE_ID = re.compile(r"^[a-z0-9][a-z0-9._-]{0,127}$")
_ACCESS_REQUEST_TOPOLOGY_CODES = {"catalog": 1, "durable": 2, "scoped": 3}
_ACCESS_REQUEST_OPERATION_CODES = {
    "catalog_discovery": 1,
    "durable_history_runtime": 2,
    "scoped_typed_observation": 3,
}
_ACCESS_REQUEST_OPERATION_TOPOLOGY = {
    "catalog_discovery": "catalog",
    "durable_history_runtime": "durable",
    "scoped_typed_observation": "scoped",
}


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
    permissions_override: Mapping[str, bool] | None = None,
) -> CompatibilityResult:
    permissions = dict(
        _PERMISSIONS[compatibility_class]
        if permissions_override is None
        else permissions_override
    )
    return CompatibilityResult(
        SUPPORT_SELECTION_CONTRACT_VERSION,
        compatibility_class,
        support_release_id,
        reason,
        permissions,
    )


def _declared_operation_permissions(release: Mapping[str, Any]) -> dict[str, bool]:
    capabilities = release.get("capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        raise SupportContractError("support release capabilities must be a non-empty list")

    seen: set[str] = set()
    levels: dict[str, list[str]] = {"catalog": [], "durable": [], "scoped": []}
    for index, capability in enumerate(capabilities):
        if not isinstance(capability, Mapping):
            raise SupportContractError(f"support capability {index} must be an object")
        capability_id = capability.get("capability_id")
        if (
            not isinstance(capability_id, str)
            or not capability_id
            or capability_id.strip() != capability_id
            or len(capability_id.encode("utf-8")) > 128
        ):
            raise SupportContractError(f"support capability {index} has an invalid id")
        if capability_id in seen:
            raise SupportContractError(f"duplicate support capability id {capability_id!r}")
        seen.add(capability_id)
        topology = capability.get("topology")
        if not isinstance(topology, str) or topology not in levels:
            raise SupportContractError(f"support capability {capability_id!r} has an unsupported topology")
        level = capability.get("level")
        if not isinstance(level, str) or level not in {"supported", "degraded", "unsupported"}:
            raise SupportContractError(f"support capability {capability_id!r} has an unsupported level")
        levels[topology].append(level)

    def fully_supported(topology: str) -> bool:
        declared = levels[topology]
        return bool(declared) and all(level == "supported" for level in declared)

    return {
        "version_probe": True,
        "catalog": fully_supported("catalog"),
        "durable": fully_supported("durable"),
        "scoped_observation": fully_supported("scoped"),
        "bounded_drift": True,
    }


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
    """Classify one native artifact without letting an unpromoted release confer support.

    A release entry is selectable when it carries ``runtime_selectable``. That
    is derived from the scope program's declared status by whoever loads the
    bundle — it is deliberately not a second status field on the release
    document, which would let a release and its declarations disagree.
    """

    release_list = list(releases)
    release_ids = [str(entry["support_release_id"]) for entry in release_list]
    if len(release_ids) != len(set(release_ids)):
        raise SupportContractError("duplicate support release id")
    declared_permissions = {
        release_id: _declared_operation_permissions(entry)
        for release_id, entry in zip(release_ids, release_list, strict=True)
    }
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
        if entry.get("runtime_selectable", False)
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
        release_id = str(entry["support_release_id"])
        return _result(
            selected,
            release_id,
            reason,
            permissions_override=declared_permissions[release_id],
        )

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
        and declared_permissions[str(entry["support_release_id"])]["catalog"]
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
            permissions_override={**_PERMISSIONS[CompatibilityClass.RECOGNIZED_UNVERIFIED], "catalog": True},
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


class AccessRequestError(ValueError):
    """A native-probe/grant or access-report retrieval request is not well-formed."""


def native_probe_grant_request_digest(value: Any) -> str:
    """Return the RFC 012A v1 digest of a fully validated native-probe/grant request."""

    try:
        request, _, _ = _validated_native_probe_grant_request(value)
        return _native_probe_grant_request_digest(request)
    except AccessRequestError:
        raise
    except (AccessReportError, KeyError, TypeError, AttributeError):
        raise AccessRequestError("native-probe/grant request is not valid JSON") from None


def access_report_retrieval_digest(value: Any) -> str:
    """Return the RFC 012A v1 digest of a fully validated access-report retrieval request."""

    try:
        request, _ = _validated_access_report_retrieval(value)
        return _access_report_retrieval_digest(request)
    except AccessRequestError:
        raise
    except (AccessReportError, KeyError, TypeError, AttributeError):
        raise AccessRequestError("access-report retrieval request is not valid JSON") from None


def _native_probe_grant_request_digest(request: Mapping[str, Any]) -> str:
    digest = hashlib.sha256()
    digest.update(b"spaghetti/rfc012a/native-probe-grant-request/v1\0")
    _hash_access_request_coordinates(digest, request, request["access_request_contract_version"])
    _hash_probe(digest, request["probe"])
    _hash_grants(digest, request["grants"])
    return f"sha256:{digest.hexdigest()}"


def _access_report_retrieval_digest(request: Mapping[str, Any]) -> str:
    digest = hashlib.sha256()
    digest.update(b"spaghetti/rfc012a/access-report-retrieval/v1\0")
    _hash_access_request_coordinates(
        digest, request, request["access_report_retrieval_contract_version"]
    )
    _request_component(digest, _request_digest_bytes(request["expected_report_digest"]))
    return f"sha256:{digest.hexdigest()}"


def parse_native_probe_grant_request(value: Any) -> dict[str, Any]:
    try:
        return _parse_native_probe_grant_request(value)
    except AccessRequestError:
        raise
    except (AccessReportError, KeyError, TypeError, AttributeError):
        raise AccessRequestError("native-probe/grant request is not valid JSON") from None


def _parse_native_probe_grant_request(value: Any) -> dict[str, Any]:
    request, probe, parsed_grants = _validated_native_probe_grant_request(value)
    expected = _native_probe_grant_request_digest(request)
    actual = f"sha256:{_request_digest_bytes(request['digest']).hex()}"
    if expected != actual:
        raise AccessRequestError("native-probe/grant request digest does not match its canonical encoding")
    request = dict(request)
    request["probe"] = probe
    request["grants"] = parsed_grants
    return request


def _validated_native_probe_grant_request(value: Any) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, Any]]]:
    request = _access_request_object(value, "native-probe/grant request")
    _assert_known_fields(
        request,
        {
            "access_request_contract_version",
            "adapter_id",
            "support_release_id",
            "support_release_digest",
            "source_declaration_digest",
            "scope_program_digest",
            "declaration_id",
            "program_id",
            "capability_topology",
            "operation",
            "selection",
            "access_policy_digest",
            "probe",
            "grants",
            "digest",
        },
        "native-probe/grant request",
    )
    _assert_required_fields(
        request,
        {
            "access_request_contract_version",
            "adapter_id",
            "support_release_id",
            "support_release_digest",
            "source_declaration_digest",
            "scope_program_digest",
            "declaration_id",
            "program_id",
            "capability_topology",
            "operation",
            "selection",
            "access_policy_digest",
            "probe",
            "grants",
            "digest",
        },
        "native-probe/grant request",
    )
    if request["access_request_contract_version"] != ACCESS_REQUEST_CONTRACT_VERSION:
        raise AccessRequestError("unsupported native-probe/grant request contract version")
    _preflight_probe_grant_collections(request)
    _validate_access_request_coordinates(request, require_program=False)
    probe = _parse_request_probe(request["probe"])
    grants = request["grants"]
    if not isinstance(grants, list):
        raise AccessRequestError("probe/grant request grants must be an array")
    if len(grants) > _MAX_ACCESS_REQUEST_GRANTS:
        raise AccessRequestError("probe/grant request exceeds the grant collection limit")
    operation = request["operation"]
    topology = request["capability_topology"]
    if _ACCESS_REQUEST_OPERATION_TOPOLOGY.get(operation) != topology:
        raise AccessRequestError("probe/grant request topology does not match its operation")
    program_id = request["program_id"]
    if not isinstance(program_id, str):
        raise AccessRequestError("request program id must be a string")
    if operation == "scoped_typed_observation":
        _access_request_identifier(program_id, "request program id")
        if not grants:
            raise AccessRequestError("scoped probe/grant request requires a bounded nonempty grant set")
        if request["selection"]["observation_contract_version"] is None:
            raise AccessRequestError("scoped probe/grant request requires a negotiated observation contract")
    elif operation in {"catalog_discovery", "durable_history_runtime"}:
        if program_id != "" or grants:
            raise AccessRequestError(
                "catalog and durable probe/grant requests cannot carry grants or a program id"
            )
        if operation == "catalog_discovery" and request["selection"]["query_pack_version"] is None:
            raise AccessRequestError("catalog discovery requires a negotiated query-pack contract")
    else:
        raise AccessRequestError(f"unsupported probe/grant request operation {operation!r}")
    encoded = [0]
    parsed_grants = [_parse_declared_grant(grant, encoded) for grant in grants]
    previous = None
    root_count = 0
    for grant in parsed_grants:
        if previous is not None and previous >= grant["relation_id"]:
            raise AccessRequestError("probe/grant relation ids must be strictly increasing")
        previous = grant["relation_id"]
        if grant["scope_root"]:
            root_count += 1
    if operation == "scoped_typed_observation" and root_count != 1:
        raise AccessRequestError("scoped probe/grant request requires exactly one scope-root grant")
    _request_digest_bytes(request["digest"])
    return request, probe, parsed_grants


def parse_access_report_retrieval(value: Any) -> dict[str, Any]:
    try:
        return _parse_access_report_retrieval(value)
    except AccessRequestError:
        raise
    except (AccessReportError, KeyError, TypeError, AttributeError):
        raise AccessRequestError("access-report retrieval request is not valid JSON") from None


def _parse_access_report_retrieval(value: Any) -> dict[str, Any]:
    request, expected_report = _validated_access_report_retrieval(value)
    expected = _access_report_retrieval_digest(request)
    actual = f"sha256:{_request_digest_bytes(request['digest']).hex()}"
    if expected != actual:
        raise AccessRequestError("access-report retrieval digest does not match its canonical encoding")
    parsed = dict(request)
    parsed["expected_report_digest"] = list(expected_report)
    return parsed


def _validated_access_report_retrieval(value: Any) -> tuple[dict[str, Any], bytes]:
    request = _access_request_object(value, "access-report retrieval request")
    _assert_known_fields(
        request,
        {
            "access_report_retrieval_contract_version",
            "adapter_id",
            "support_release_id",
            "support_release_digest",
            "source_declaration_digest",
            "scope_program_digest",
            "declaration_id",
            "program_id",
            "capability_topology",
            "operation",
            "selection",
            "access_policy_digest",
            "expected_report_digest",
            "digest",
        },
        "access-report retrieval request",
    )
    _assert_required_fields(
        request,
        {
            "access_report_retrieval_contract_version",
            "adapter_id",
            "support_release_id",
            "support_release_digest",
            "source_declaration_digest",
            "scope_program_digest",
            "declaration_id",
            "program_id",
            "capability_topology",
            "operation",
            "selection",
            "access_policy_digest",
            "expected_report_digest",
            "digest",
        },
        "access-report retrieval request",
    )
    if request["access_report_retrieval_contract_version"] != ACCESS_REPORT_RETRIEVAL_CONTRACT_VERSION:
        raise AccessRequestError("unsupported access-report retrieval contract version")
    _validate_access_request_coordinates(request, require_program=True)
    if request["capability_topology"] != "scoped" or request["operation"] != "scoped_typed_observation":
        raise AccessRequestError("access-report retrieval is scoped observation only")
    if request["selection"]["observation_contract_version"] is None:
        raise AccessRequestError("access-report retrieval requires a negotiated observation contract")
    expected_report = _nonzero_digest_bytes(request["expected_report_digest"], "expected report digest")
    _request_digest_bytes(request["digest"])
    return request, expected_report


def _access_request_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or type(value) is not dict:
        raise AccessRequestError(f"{label} must be a plain object")
    return value


def _assert_known_fields(value: Mapping[str, Any], fields: set[str], label: str) -> None:
    extra = set(value) - fields
    if extra:
        raise AccessRequestError(f"{label} contains an unknown field")


def _assert_required_fields(value: Mapping[str, Any], fields: set[str], label: str) -> None:
    if any(field not in value for field in fields):
        raise AccessRequestError(f"{label} is missing a required field")


def _require_field(value: Mapping[str, Any], field: str, label: str) -> Any:
    if field not in value:
        raise AccessRequestError(f"{label} is missing a required field")
    return value[field]


def _access_request_identifier(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise AccessRequestError(f"{label} must be a machine identifier")
    if not value or len(value) > _MAX_ACCESS_REQUEST_IDENTIFIER_BYTES or _REQUEST_MACHINE_ID.fullmatch(value) is None:
        raise AccessRequestError(f"{label} must be a machine identifier")
    if len(value.encode("utf-8")) > _MAX_ACCESS_REQUEST_IDENTIFIER_BYTES:
        raise AccessRequestError(f"{label} must be a machine identifier")
    return value


def _charge_encoded_bytes(budget: list[int], value: str) -> None:
    budget[0] += len(value)
    if budget[0] > _MAX_ACCESS_REQUEST_ENCODED_BYTES:
        raise AccessRequestError("access request exceeds the encoded-byte limit")


def _nonzero_digest_bytes(value: Any, label: str) -> bytes:
    digest = _request_digest_bytes(value)
    if all(byte == 0 for byte in digest):
        raise AccessRequestError(f"{label} must be a nonzero 32-byte digest")
    return digest


def _request_digest_bytes(value: Any) -> bytes:
    if (
        not isinstance(value, list)
        or len(value) != 32
        or any(not isinstance(item, int) or isinstance(item, bool) or not 0 <= item <= 255 for item in value)
    ):
        raise AccessRequestError("request digest must contain 32 bytes")
    return bytes(value)


def _request_text(value: Any) -> bytes:
    if not isinstance(value, str):
        raise AccessRequestError("access request is not valid JSON")
    return value.encode("utf-8")


def _request_component(digest: Any, value: bytes) -> None:
    _request_u64(digest, len(value))
    digest.update(value)


def _request_u32(digest: Any, value: Any) -> None:
    _request_unsigned(digest, value, 4)


def _request_u64(digest: Any, value: Any) -> None:
    _request_unsigned(digest, value, 8)


def _request_unsigned(digest: Any, value: Any, width: int) -> None:
    if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value < 1 << (width * 8):
        raise AccessRequestError("access request is not valid JSON")
    digest.update(value.to_bytes(width, "big"))


def _preflight_identifier_length(value: Any, label: str) -> None:
    if isinstance(value, str) and len(value) > _MAX_ACCESS_REQUEST_IDENTIFIER_BYTES:
        raise AccessRequestError(f"{label} must be a machine identifier")


def _preflight_request_selection_bounds(selection: Any) -> None:
    if not isinstance(selection, dict) or type(selection) is not dict:
        raise AccessRequestError("contract version selection must be a plain object")
    families = selection.get("fact_family_versions")
    if isinstance(families, dict) and len(families) > _MAX_ACCESS_REQUEST_FACT_FAMILIES:
        raise AccessRequestError("selected fact families exceed the collection limit")
    if isinstance(families, dict):
        for family in families:
            _preflight_identifier_length(family, "selected fact family")


def _preflight_probe_grant_collections(request: Mapping[str, Any]) -> None:
    _preflight_identifier_length(request.get("program_id"), "request program id")
    probe = request.get("probe")
    if isinstance(probe, dict):
        _preflight_identifier_length(probe.get("family"), "probed artifact family")
        _preflight_identifier_length(probe.get("platform"), "probed artifact platform")
        _preflight_identifier_length(probe.get("version"), "probed artifact version")
        markers = probe.get("markers")
        if isinstance(markers, list):
            if len(markers) > _MAX_ACCESS_REQUEST_MARKERS:
                raise AccessRequestError("native probe exceeds the marker collection limit")
            for marker in markers:
                _preflight_identifier_length(marker, "probed native marker")
    _preflight_request_selection_bounds(request.get("selection"))
    grants = request.get("grants")
    if not isinstance(grants, list):
        return
    if len(grants) > _MAX_ACCESS_REQUEST_GRANTS:
        raise AccessRequestError("probe/grant request exceeds the grant collection limit")
    encoded = 0
    for grant in grants:
        if not isinstance(grant, dict):
            continue
        for field_name, label in (("relation_id", "grant relation id"), ("access_root", "grant access root")):
            value = grant.get(field_name)
            _preflight_identifier_length(value, label)
            if isinstance(value, str):
                encoded += len(value)
                if encoded > _MAX_ACCESS_REQUEST_ENCODED_BYTES:
                    raise AccessRequestError("access request exceeds the encoded-byte limit")
        names = grant.get("identity_input_names")
        if not isinstance(names, list):
            continue
        if len(names) > _MAX_ACCESS_REQUEST_IDENTITY_INPUTS:
            raise AccessRequestError("grant identity inputs exceed the collection limit")
        for name in names:
            _preflight_identifier_length(name, "grant identity input")
            if isinstance(name, str):
                encoded += len(name)
                if encoded > _MAX_ACCESS_REQUEST_ENCODED_BYTES:
                    raise AccessRequestError("access request exceeds the encoded-byte limit")


def _validate_access_request_coordinates(request: Mapping[str, Any], *, require_program: bool) -> None:
    _access_request_identifier(_require_field(request, "adapter_id", "native-probe/grant request"), "request adapter id")
    _access_request_identifier(
        _require_field(request, "support_release_id", "native-probe/grant request"),
        "request support release id",
    )
    _access_request_identifier(
        _require_field(request, "declaration_id", "native-probe/grant request"),
        "request declaration id",
    )
    if require_program:
        _access_request_identifier(
            _require_field(request, "program_id", "native-probe/grant request"),
            "request program id",
        )
    _nonzero_digest_bytes(
        _require_field(request, "support_release_digest", "native-probe/grant request"),
        "support release digest",
    )
    _nonzero_digest_bytes(
        _require_field(request, "source_declaration_digest", "native-probe/grant request"),
        "source declaration digest",
    )
    _nonzero_digest_bytes(
        _require_field(request, "scope_program_digest", "native-probe/grant request"),
        "scope program digest",
    )
    _nonzero_digest_bytes(
        _require_field(request, "access_policy_digest", "native-probe/grant request"),
        "access policy digest",
    )
    topology = _require_field(request, "capability_topology", "native-probe/grant request")
    operation = _require_field(request, "operation", "native-probe/grant request")
    if topology not in _ACCESS_REQUEST_TOPOLOGY_CODES:
        raise AccessRequestError("unsupported request capability topology")
    if operation not in _ACCESS_REQUEST_OPERATION_CODES:
        raise AccessRequestError("unsupported request operation")
    _parse_request_selection(_require_field(request, "selection", "native-probe/grant request"), operation)


def _parse_request_selection(value: Any, operation: Any) -> dict[str, Any]:
    selection = _access_request_object(value, "contract version selection")
    _assert_known_fields(
        selection,
        {
            "selection_contract_version",
            "model_major",
            "external_entity_reference_version",
            "semantic_revision_reference_version",
            "coverage_contract_version",
            "fact_family_versions",
            "query_pack_version",
            "observation_contract_version",
        },
        "contract version selection",
    )
    _assert_required_fields(
        selection,
        {
            "selection_contract_version",
            "model_major",
            "external_entity_reference_version",
            "semantic_revision_reference_version",
            "coverage_contract_version",
            "fact_family_versions",
            "query_pack_version",
            "observation_contract_version",
        },
        "contract version selection",
    )
    if selection["selection_contract_version"] != CONTRACT_VERSION_SELECTION_VERSION:
        raise AccessRequestError("unsupported contract-version selection version")
    for label in (
        "model_major",
        "external_entity_reference_version",
        "semantic_revision_reference_version",
        "coverage_contract_version",
    ):
        _positive_u32(selection[label], f"selected {label.replace('_', ' ')}")
    families = selection["fact_family_versions"]
    if not isinstance(families, dict) or type(families) is not dict:
        raise AccessRequestError("selected fact families must be a plain object")
    if len(families) > _MAX_ACCESS_REQUEST_FACT_FAMILIES:
        raise AccessRequestError("selected fact families exceed the collection limit")
    encoded = [0]
    for family, version in families.items():
        parsed_family = _access_request_identifier(family, "selected fact family")
        _charge_encoded_bytes(encoded, parsed_family)
        _positive_u32(version, "selected fact-family version")
    for field_name, label in (
        ("query_pack_version", "selected query pack version"),
        ("observation_contract_version", "selected observation contract version"),
    ):
        version = selection[field_name]
        if version is not None:
            _positive_u32(version, label)
    if operation == "catalog_discovery" and selection["query_pack_version"] is None:
        raise AccessRequestError("catalog discovery requires a negotiated query-pack contract")
    if operation == "scoped_typed_observation" and selection["observation_contract_version"] is None:
        raise AccessRequestError("scoped probe/grant request requires a negotiated observation contract")
    return selection


def _parse_request_probe(value: Any) -> dict[str, Any]:
    probe = _access_request_object(value, "native artifact probe")
    _assert_known_fields(
        probe,
        {"family", "platform", "version", "markers", "contradictory_markers"},
        "native artifact probe",
    )
    _assert_required_fields(
        probe,
        {"family", "platform", "version", "markers", "contradictory_markers"},
        "native artifact probe",
    )
    family = _access_request_identifier(probe["family"], "probed artifact family")
    platform = _access_request_identifier(probe["platform"], "probed artifact platform")
    encoded = [0]
    _charge_encoded_bytes(encoded, family)
    _charge_encoded_bytes(encoded, platform)
    version = probe["version"]
    if version is not None:
        version = _access_request_identifier(version, "probed artifact version")
        _charge_encoded_bytes(encoded, version)
    markers = probe["markers"]
    if not isinstance(markers, list):
        raise AccessRequestError("probed native markers must be an array")
    if len(markers) > _MAX_ACCESS_REQUEST_MARKERS:
        raise AccessRequestError("native probe exceeds the marker collection limit")
    parsed_markers: list[str] = []
    for marker in markers:
        parsed = _access_request_identifier(marker, "probed native marker")
        _charge_encoded_bytes(encoded, parsed)
        parsed_markers.append(parsed)
    parsed_markers = sorted(set(parsed_markers))
    if not isinstance(probe["contradictory_markers"], bool):
        raise AccessRequestError("contradictory_markers must be boolean")
    return {
        "family": family,
        "platform": platform,
        "version": version,
        "markers": parsed_markers,
        "contradictory_markers": probe["contradictory_markers"],
    }


def _parse_declared_grant(value: Any, encoded: list[int]) -> dict[str, Any]:
    grant = _access_request_object(value, "declared known-object grant")
    _assert_known_fields(
        grant,
        {"relation_id", "scope_root", "access_root", "identity_input_names"},
        "declared known-object grant",
    )
    _assert_required_fields(
        grant,
        {"relation_id", "scope_root", "access_root", "identity_input_names"},
        "declared known-object grant",
    )
    relation_id = _access_request_identifier(grant["relation_id"], "grant relation id")
    access_root = _access_request_identifier(grant["access_root"], "grant access root")
    _charge_encoded_bytes(encoded, relation_id)
    _charge_encoded_bytes(encoded, access_root)
    names = grant["identity_input_names"]
    if not isinstance(names, list) or not names:
        raise AccessRequestError("grant identity input list must not be empty")
    if len(names) > _MAX_ACCESS_REQUEST_IDENTITY_INPUTS:
        raise AccessRequestError("grant identity inputs exceed the collection limit")
    parsed_names: list[str] = []
    seen: set[str] = set()
    for name in names:
        parsed = _access_request_identifier(name, "grant identity input")
        _charge_encoded_bytes(encoded, parsed)
        if parsed in seen:
            raise AccessRequestError("grant identity input contains duplicate value")
        seen.add(parsed)
        parsed_names.append(parsed)
    if not isinstance(grant["scope_root"], bool):
        raise AccessRequestError("grant scope_root must be boolean")
    return {
        "relation_id": relation_id,
        "scope_root": grant["scope_root"],
        "access_root": access_root,
        "identity_input_names": parsed_names,
    }


def _positive_u32(value: Any, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0 or value > 0xFFFFFFFF:
        raise AccessRequestError(f"{label} must be a positive u32")
    return value


def _hash_access_request_coordinates(digest: Any, request: Mapping[str, Any], contract_version: Any) -> None:
    _request_u32(digest, contract_version)
    _request_component(digest, _request_text(request["adapter_id"]))
    _request_component(digest, _request_text(request["support_release_id"]))
    _request_component(digest, _request_digest_bytes(request["support_release_digest"]))
    _request_component(digest, _request_digest_bytes(request["source_declaration_digest"]))
    _request_component(digest, _request_digest_bytes(request["scope_program_digest"]))
    _request_component(digest, _request_text(request["declaration_id"]))
    _request_component(digest, _request_text(request["program_id"]))
    digest.update(bytes((_ACCESS_REQUEST_TOPOLOGY_CODES[request["capability_topology"]],)))
    digest.update(bytes((_ACCESS_REQUEST_OPERATION_CODES[request["operation"]],)))
    _hash_selection(digest, request["selection"])
    _request_component(digest, _request_digest_bytes(request["access_policy_digest"]))


def _hash_selection(digest: Any, selection: Mapping[str, Any]) -> None:
    _request_u32(digest, selection["selection_contract_version"])
    _request_u32(digest, selection["model_major"])
    _request_u32(digest, selection["external_entity_reference_version"])
    _request_u32(digest, selection["semantic_revision_reference_version"])
    _request_u32(digest, selection["coverage_contract_version"])
    families = selection["fact_family_versions"]
    _request_u64(digest, len(families))
    for family in sorted(families):
        _request_component(digest, _request_text(family))
        _request_u32(digest, families[family])
    for field_name in ("query_pack_version", "observation_contract_version"):
        version = selection[field_name]
        if version is None:
            digest.update(b"\x00")
        else:
            digest.update(b"\x01")
            _request_u32(digest, version)


def _hash_probe(digest: Any, probe: Mapping[str, Any]) -> None:
    _request_component(digest, _request_text(probe["family"]))
    _request_component(digest, _request_text(probe["platform"]))
    version = probe["version"]
    if version is None:
        digest.update(b"\x00")
    else:
        digest.update(b"\x01")
        _request_component(digest, _request_text(version))
    markers = sorted(set(probe["markers"]))
    _request_u64(digest, len(markers))
    for marker in markers:
        _request_component(digest, _request_text(marker))
    digest.update(bytes((1 if probe["contradictory_markers"] else 0,)))


def _hash_grants(digest: Any, grants: Sequence[Mapping[str, Any]]) -> None:
    _request_u64(digest, len(grants))
    for grant in grants:
        _request_component(digest, _request_text(grant["relation_id"]))
        digest.update(bytes((1 if grant["scope_root"] else 0,)))
        _request_component(digest, _request_text(grant["access_root"]))
        names = grant["identity_input_names"]
        _request_u64(digest, len(names))
        for name in names:
            _request_component(digest, _request_text(name))


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
