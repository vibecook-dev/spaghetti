//! Store-free RFC 012B initial catalog publication assembly.
//!
//! This module validates the complete semantic payload that a later B3 writer
//! may commit atomically. It deliberately owns no SQLite, source reads,
//! snapshot transition, serialization, or public transport surface.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use super::evidence::{
    CatalogAssertionKey, CatalogEntityKind, CatalogEntityRef, CatalogEvidenceOwner, CatalogReducer,
    CatalogReducerPublication, CatalogReducerPublicationLimits,
};
use super::{
    validate_identifier, CatalogContractError, CatalogCoveragePlan, CatalogCoveragePlanId,
    CatalogCoveragePlanSource, CatalogCoverageScope, CatalogReadinessPhase,
    CatalogReadinessSnapshot, CATALOG_PROJECTION_PACK_ID, CATALOG_QUERY_PACK_CONTRACT_VERSION,
    DIGEST_BYTES,
};
use crate::adapter::{
    ContractVersionSelection, CoverageDomain, CoverageSetCompleteness, SourceCoverageSet,
    CONTRACT_VERSION_SELECTION_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
};

pub(crate) const CATALOG_INITIAL_PUBLICATION_CONTRACT_VERSION: u32 = 1;

const MAX_PUBLICATION_MEMBERS: usize = 1_000_000;
const MAX_SELECTED_FACT_FAMILIES: usize = 4_096;

macro_rules! private_digest_type {
    ($name:ident, $label:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub(crate) struct $name([u8; DIGEST_BYTES]);

        impl $name {
            pub(crate) fn from_digest(bytes: [u8; DIGEST_BYTES]) -> Self {
                Self(bytes)
            }

            fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&format_args!(
                        "{}:{}",
                        $label,
                        URL_SAFE_NO_PAD.encode(self.0)
                    ))
                    .finish()
            }
        }
    };
}

private_digest_type!(CatalogPublicationMemberRef, "catalog-member-v1");
private_digest_type!(CatalogSourceMembershipRevision, "catalog-membership-v1");
private_digest_type!(
    CatalogSourceCompletionRevision,
    "catalog-component-completion-v1"
);
private_digest_type!(CatalogCompleteSourceDigest, "catalog-complete-source-v1");
private_digest_type!(
    CatalogInitialPublicationDigest,
    "catalog-initial-publication-v1"
);

impl fmt::Display for CatalogInitialPublicationDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "catalog-initial-publication-v1:{}",
            URL_SAFE_NO_PAD.encode(self.0)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogPublicationLimits {
    pub max_members: usize,
    pub reducer: CatalogReducerPublicationLimits,
}

impl CatalogPublicationLimits {
    pub(crate) fn new(
        max_members: usize,
        max_reducer_entries: usize,
        max_rows: usize,
    ) -> Result<Self, CatalogContractError> {
        if max_members == 0 || max_members > MAX_PUBLICATION_MEMBERS {
            return Err(CatalogContractError::invalid(format!(
                "catalog publication member bound must be within 1..={MAX_PUBLICATION_MEMBERS}"
            )));
        }
        Ok(Self {
            max_members,
            reducer: CatalogReducerPublicationLimits::new(max_reducer_entries, max_rows)?,
        })
    }
}

impl Default for CatalogPublicationLimits {
    fn default() -> Self {
        Self {
            max_members: MAX_PUBLICATION_MEMBERS,
            reducer: CatalogReducerPublicationLimits::default(),
        }
    }
}

/// Source-neutral projection of one checked B2 complete Library assembly.
/// It intentionally retains only opaque membership identity and RFC 012A/B
/// contract values. Construction validates the same exact policy/declaration,
/// coverage, and selection bindings that readiness will later publish.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogCompleteSourceAssembly {
    plan_source: CatalogCoveragePlanSource,
    contract_selection: ContractVersionSelection,
    member_identity_contract_id: String,
    membership_revision: CatalogSourceMembershipRevision,
    component_completion_revision: CatalogSourceCompletionRevision,
    member_refs: Vec<CatalogPublicationMemberRef>,
    source_coverage: SourceCoverageSet,
    digest: CatalogCompleteSourceDigest,
}

impl CatalogCompleteSourceAssembly {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_complete_library_coverage(
        plan_source: CatalogCoveragePlanSource,
        contract_selection: ContractVersionSelection,
        member_identity_contract_id: impl Into<String>,
        membership_revision: CatalogSourceMembershipRevision,
        component_completion_revision: CatalogSourceCompletionRevision,
        mut member_refs: Vec<CatalogPublicationMemberRef>,
        mut source_coverage: SourceCoverageSet,
    ) -> Result<Self, CatalogContractError> {
        validate_contract_selection(&contract_selection)?;
        plan_source.validate()?;
        let member_identity_contract_id = member_identity_contract_id.into();
        validate_identifier(
            "catalog member identity contract id",
            &member_identity_contract_id,
        )?;
        if membership_revision.as_bytes().iter().all(|byte| *byte == 0)
            || component_completion_revision
                .as_bytes()
                .iter()
                .all(|byte| *byte == 0)
        {
            return Err(CatalogContractError::invalid(
                "catalog source membership and component completion revisions must be nonzero",
            ));
        }
        if member_refs.len() > MAX_PUBLICATION_MEMBERS {
            return Err(CatalogContractError::invalid(
                "catalog complete source exceeds the bounded publication member ceiling",
            ));
        }
        if member_refs
            .iter()
            .any(|member_ref| member_ref.as_bytes().iter().all(|byte| *byte == 0))
        {
            return Err(CatalogContractError::invalid(
                "catalog complete source contains an invalid zero member reference",
            ));
        }
        member_refs.sort_unstable();
        if member_refs.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(CatalogContractError::invalid(
                "catalog complete source contains duplicate member references",
            ));
        }
        validate_complete_source_coverage(&plan_source, &contract_selection, &source_coverage)?;
        source_coverage
            .points
            .sort_by_key(|point| (point.stream_key, point.object_key, point.generation));
        source_coverage.explicit_absence_or_deletion.sort();
        source_coverage.explicit_errors.sort();

        let digest = derive_complete_source_digest(
            &plan_source,
            &contract_selection,
            &member_identity_contract_id,
            membership_revision,
            component_completion_revision,
            &member_refs,
            &source_coverage,
        )?;
        Ok(Self {
            plan_source,
            contract_selection,
            member_identity_contract_id,
            membership_revision,
            component_completion_revision,
            member_refs,
            source_coverage,
            digest,
        })
    }

    pub(crate) fn plan_source(&self) -> &CatalogCoveragePlanSource {
        &self.plan_source
    }

    pub(crate) fn member_identity_contract_id(&self) -> &str {
        &self.member_identity_contract_id
    }

    pub(crate) fn member_count(&self) -> usize {
        self.member_refs.len()
    }

    pub(crate) fn membership_revision(&self) -> CatalogSourceMembershipRevision {
        self.membership_revision
    }

    pub(crate) fn component_completion_revision(&self) -> CatalogSourceCompletionRevision {
        self.component_completion_revision
    }

    pub(crate) fn source_coverage(&self) -> &SourceCoverageSet {
        &self.source_coverage
    }

    pub(crate) fn member_binding(
        &self,
        member_ref: CatalogPublicationMemberRef,
        assertion_key: CatalogAssertionKey,
        session_ref: CatalogEntityRef,
    ) -> Result<CatalogPublicationMemberBinding, CatalogContractError> {
        if self.member_refs.binary_search(&member_ref).is_err() {
            return Err(CatalogContractError::invalid(
                "catalog member binding names a member outside its complete source assembly",
            ));
        }
        CatalogPublicationMemberBinding::new(
            self.plan_source.clone(),
            member_ref,
            assertion_key,
            session_ref,
        )
    }
}

impl fmt::Debug for CatalogCompleteSourceAssembly {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogCompleteSourceAssembly")
            .field("adapter_id", &self.plan_source.adapter_id)
            .field("source_instance_key", &self.plan_source.source_instance_key)
            .field("support_release_id", &self.plan_source.support_release_id)
            .field(
                "member_identity_contract_id",
                &self.member_identity_contract_id,
            )
            .field("membership_revision", &self.membership_revision)
            .field(
                "component_completion_revision",
                &self.component_completion_revision,
            )
            .field("member_count", &self.member_refs.len())
            .field("digest", &self.digest)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogPublicationMemberBinding {
    source: CatalogCoveragePlanSource,
    member_ref: CatalogPublicationMemberRef,
    assertion_key: CatalogAssertionKey,
    session_ref: CatalogEntityRef,
}

impl CatalogPublicationMemberBinding {
    fn new(
        source: CatalogCoveragePlanSource,
        member_ref: CatalogPublicationMemberRef,
        assertion_key: CatalogAssertionKey,
        session_ref: CatalogEntityRef,
    ) -> Result<Self, CatalogContractError> {
        source.validate()?;
        session_ref.validate()?;
        if session_ref.kind != CatalogEntityKind::Session {
            return Err(CatalogContractError::invalid(
                "catalog membership must bind to a concrete base session assertion",
            ));
        }
        Ok(Self {
            source,
            member_ref,
            assertion_key,
            session_ref,
        })
    }

    fn coordinate(&self) -> (&CatalogCoveragePlanSource, CatalogPublicationMemberRef) {
        (&self.source, self.member_ref)
    }
}

impl fmt::Debug for CatalogPublicationMemberBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogPublicationMemberBinding")
            .field("adapter_id", &self.source.adapter_id)
            .field("source_instance_key", &self.source.source_instance_key)
            .field("member_ref", &self.member_ref)
            .field("assertion_key", &"<opaque>")
            .field("session_ref", &"<opaque>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogInitialBuildExpectation {
    pub coverage_plan_id: CatalogCoveragePlanId,
    pub desired_contract_version: u32,
    pub epoch: u64,
    pub attempt: u64,
}

/// Fully checked, store-free input for one future atomic initial catalog
/// publication. The raw reducer evidence and rows remain private and Debug is
/// redacted; the later writer must consume this checked value rather than
/// accepting independent coverage, row, and readiness inputs.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogInitialPublicationAssembly {
    contract_version: u32,
    build: CatalogInitialBuildExpectation,
    contract_selection: ContractVersionSelection,
    member_identity_contract_id: Option<String>,
    sources: Vec<CatalogCompleteSourceAssembly>,
    member_bindings: Vec<CatalogPublicationMemberBinding>,
    reducer: CatalogReducerPublication,
    limits: CatalogPublicationLimits,
    digest: CatalogInitialPublicationDigest,
}

impl CatalogInitialPublicationAssembly {
    pub(crate) fn assemble(
        plan: &CatalogCoveragePlan,
        readiness: &CatalogReadinessSnapshot,
        contract_selection: ContractVersionSelection,
        mut sources: Vec<CatalogCompleteSourceAssembly>,
        reducer: &CatalogReducer,
        mut member_bindings: Vec<CatalogPublicationMemberBinding>,
        limits: CatalogPublicationLimits,
    ) -> Result<Self, CatalogContractError> {
        let build = validate_initial_build(plan, readiness, &contract_selection)?;
        CatalogPublicationLimits::new(
            limits.max_members,
            limits.reducer.max_reducer_entries,
            limits.reducer.max_rows,
        )?;

        let planned_source_count = plan
            .required_sources
            .len()
            .checked_add(plan.optional_sources.len())
            .ok_or_else(|| CatalogContractError::invalid("catalog source count overflow"))?;
        if sources.len() > planned_source_count {
            return Err(CatalogContractError::invalid(
                "catalog publication contains more complete sources than its frozen plan",
            ));
        }
        sources.sort_by(|left, right| left.plan_source.cmp(&right.plan_source));
        if sources
            .windows(2)
            .any(|pair| pair[0].plan_source == pair[1].plan_source)
        {
            return Err(CatalogContractError::invalid(
                "catalog publication contains a duplicate complete source assembly",
            ));
        }
        validate_plan_sources(plan, &contract_selection, &sources)?;

        let total_members = sources.iter().try_fold(0_usize, |count, source| {
            count.checked_add(source.member_refs.len()).ok_or_else(|| {
                CatalogContractError::invalid("catalog publication member count overflow")
            })
        })?;
        if total_members > limits.max_members {
            return Err(CatalogContractError::invalid(
                "catalog publication exceeds its bounded member ceiling",
            ));
        }
        if member_bindings.len() != total_members {
            return Err(CatalogContractError::invalid(
                "catalog publication requires exactly one session binding for every admitted member",
            ));
        }
        let member_identity_contract_id = sources
            .first()
            .map(|source| source.member_identity_contract_id.clone());
        if let Some(expected) = &member_identity_contract_id {
            if sources
                .iter()
                .any(|source| source.member_identity_contract_id != *expected)
            {
                return Err(CatalogContractError::invalid(
                    "catalog publication sources disagree on the member identity contract",
                ));
            }
        }

        let reducer = reducer.freeze_for_initial_publication(limits.reducer)?;
        validate_covered_reducer(&sources, &reducer)?;
        member_bindings.sort_by(|left, right| left.coordinate().cmp(&right.coordinate()));
        validate_member_bindings(&sources, &reducer, &member_bindings)?;

        let digest = derive_initial_publication_digest(
            build,
            &contract_selection,
            member_identity_contract_id.as_deref(),
            &sources,
            &member_bindings,
            &reducer,
            limits,
        );
        Ok(Self {
            contract_version: CATALOG_INITIAL_PUBLICATION_CONTRACT_VERSION,
            build,
            contract_selection,
            member_identity_contract_id,
            sources,
            member_bindings,
            reducer,
            limits,
            digest,
        })
    }

    pub(crate) fn build(&self) -> CatalogInitialBuildExpectation {
        self.build
    }

    pub(crate) fn digest(&self) -> CatalogInitialPublicationDigest {
        self.digest
    }

    pub(crate) fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub(crate) fn member_count(&self) -> usize {
        self.member_bindings.len()
    }

    pub(crate) fn project_row_count(&self) -> usize {
        self.reducer.project_row_count()
    }

    pub(crate) fn session_row_count(&self) -> usize {
        self.reducer.session_row_count()
    }

    pub(crate) fn tombstone_count(&self) -> usize {
        self.reducer.tombstone_count()
    }
}

impl fmt::Debug for CatalogInitialPublicationAssembly {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogInitialPublicationAssembly")
            .field("contract_version", &self.contract_version)
            .field("build", &self.build)
            .field(
                "member_identity_contract_id",
                &self.member_identity_contract_id,
            )
            .field("source_count", &self.sources.len())
            .field("member_count", &self.member_bindings.len())
            .field("project_row_count", &self.reducer.project_row_count())
            .field("session_row_count", &self.reducer.session_row_count())
            .field("tombstone_count", &self.reducer.tombstone_count())
            .field("reducer_revision", &self.reducer.revision())
            .field("digest", &self.digest)
            .finish()
    }
}

fn validate_contract_selection(
    selection: &ContractVersionSelection,
) -> Result<(), CatalogContractError> {
    if selection.selection_contract_version != CONTRACT_VERSION_SELECTION_VERSION
        || selection.model_major == 0
        || selection.external_entity_reference_version == 0
        || selection.semantic_revision_reference_version == 0
        || selection.coverage_contract_version != SOURCE_COVERAGE_CONTRACT_VERSION
        || selection.query_pack_version != Some(CATALOG_QUERY_PACK_CONTRACT_VERSION)
        || selection.observation_contract_version == Some(0)
        || selection.fact_family_versions.len() > MAX_SELECTED_FACT_FAMILIES
    {
        return Err(CatalogContractError::invalid(
            "catalog publication requires a valid exact RFC 012A/B contract selection",
        ));
    }
    for (family, version) in &selection.fact_family_versions {
        validate_identifier("selected catalog fact family", family)?;
        if *version == 0 {
            return Err(CatalogContractError::invalid(
                "selected catalog fact-family versions must be greater than zero",
            ));
        }
    }
    Ok(())
}

fn validate_complete_source_coverage(
    plan_source: &CatalogCoveragePlanSource,
    selection: &ContractVersionSelection,
    coverage: &SourceCoverageSet,
) -> Result<(), CatalogContractError> {
    coverage.validate().map_err(|error| {
        CatalogContractError::invalid(format!("invalid complete catalog source coverage: {error}"))
    })?;
    if coverage.coverage_domain
        != (CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
            version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
        })
        || coverage.completeness != CoverageSetCompleteness::Complete
        || coverage.scope.root_entity_key.is_some()
        || !plan_source.matches_coverage(coverage)
        || selection.coverage_contract_version != SOURCE_COVERAGE_CONTRACT_VERSION
        || selection.query_pack_version != Some(CATALOG_QUERY_PACK_CONTRACT_VERSION)
    {
        return Err(CatalogContractError::invalid(
            "catalog source assembly is not complete Library coverage for its exact plan binding",
        ));
    }
    Ok(())
}

fn validate_initial_build(
    plan: &CatalogCoveragePlan,
    readiness: &CatalogReadinessSnapshot,
    selection: &ContractVersionSelection,
) -> Result<CatalogInitialBuildExpectation, CatalogContractError> {
    plan.validate()?;
    readiness.validate_against(plan)?;
    validate_contract_selection(selection)?;
    if plan.scope != CatalogCoverageScope::Library
        || readiness.state != CatalogReadinessPhase::Building
        || readiness.desired_contract_version != CATALOG_QUERY_PACK_CONTRACT_VERSION
        || selection.query_pack_version != Some(readiness.desired_contract_version)
        || readiness.epoch != 1
        || readiness.attempt != 1
        || readiness.completed_contract_version.is_some()
        || readiness.complete_through_commit.is_some()
        || readiness.last_complete_snapshot.is_some()
        || readiness.refreshing_from_snapshot.is_some()
        || !readiness.source_coverage.is_empty()
        || readiness.reason.is_some()
    {
        return Err(CatalogContractError::invalid(
            "initial catalog publication requires the exact empty durable Library Building expectation",
        ));
    }
    Ok(CatalogInitialBuildExpectation {
        coverage_plan_id: plan.coverage_plan_id,
        desired_contract_version: readiness.desired_contract_version,
        epoch: readiness.epoch,
        attempt: readiness.attempt,
    })
}

fn validate_plan_sources(
    plan: &CatalogCoveragePlan,
    selection: &ContractVersionSelection,
    sources: &[CatalogCompleteSourceAssembly],
) -> Result<(), CatalogContractError> {
    for source in sources {
        if &source.contract_selection != selection {
            return Err(CatalogContractError::invalid(
                "catalog complete source assembly uses a different negotiated selection",
            ));
        }
        validate_complete_source_coverage(
            &source.plan_source,
            &source.contract_selection,
            &source.source_coverage,
        )?;
        if !plan.required_sources.contains(&source.plan_source)
            && !plan.optional_sources.contains(&source.plan_source)
        {
            return Err(CatalogContractError::invalid(
                "catalog complete source assembly is outside the frozen coverage plan",
            ));
        }
    }
    if plan
        .required_sources
        .iter()
        .any(|required| !sources.iter().any(|source| source.plan_source == *required))
    {
        return Err(CatalogContractError::invalid(
            "catalog publication is missing a required complete source assembly",
        ));
    }
    Ok(())
}

fn live_coverage_owners(
    sources: &[CatalogCompleteSourceAssembly],
) -> Result<BTreeSet<CatalogEvidenceOwner>, CatalogContractError> {
    let mut owners = BTreeSet::new();
    for source in sources {
        for point in &source.source_coverage.points {
            let owner = CatalogEvidenceOwner::new(
                point.adapter_id.clone(),
                point.source_instance_key,
                point.stream_key,
                point.object_key,
                point.generation,
            )?;
            if !owners.insert(owner) {
                return Err(CatalogContractError::invalid(
                    "catalog publication contains duplicate live coverage coordinates",
                ));
            }
        }
    }
    Ok(owners)
}

fn absent_coverage_owners(
    sources: &[CatalogCompleteSourceAssembly],
) -> Result<BTreeSet<CatalogEvidenceOwner>, CatalogContractError> {
    let mut owners = BTreeSet::new();
    for source in sources {
        for absence in &source.source_coverage.explicit_absence_or_deletion {
            let owner = CatalogEvidenceOwner::new(
                source.source_coverage.scope.adapter_id.clone(),
                source.source_coverage.scope.source_instance_key,
                absence.stream_key,
                absence.object_key,
                absence.generation,
            )?;
            if !owners.insert(owner) {
                return Err(CatalogContractError::invalid(
                    "catalog publication contains duplicate absent coverage coordinates",
                ));
            }
        }
    }
    Ok(owners)
}

fn validate_covered_reducer(
    sources: &[CatalogCompleteSourceAssembly],
    reducer: &CatalogReducerPublication,
) -> Result<(), CatalogContractError> {
    let live = live_coverage_owners(sources)?;
    let absent = absent_coverage_owners(sources)?;
    let every_live_owner = reducer
        .projects
        .iter()
        .map(|stored| &stored.fact.owner)
        .chain(reducer.sessions.iter().map(|stored| &stored.fact.owner))
        .chain(reducer.associations.iter().map(|stored| &stored.fact.owner))
        .chain(reducer.locators.iter().map(|stored| &stored.fact.owner))
        .chain(
            reducer
                .identity_relations
                .iter()
                .map(|stored| &stored.fact.owner),
        );
    if every_live_owner
        .into_iter()
        .any(|owner| !live.contains(owner))
    {
        return Err(CatalogContractError::invalid(
            "catalog publication contains live evidence outside complete source coverage",
        ));
    }
    if reducer
        .retracted_owners
        .iter()
        .any(|evidence| !absent.contains(&evidence.owner))
    {
        return Err(CatalogContractError::invalid(
            "catalog publication contains retracted evidence without exact absence coverage",
        ));
    }
    Ok(())
}

fn validate_member_bindings(
    sources: &[CatalogCompleteSourceAssembly],
    reducer: &CatalogReducerPublication,
    bindings: &[CatalogPublicationMemberBinding],
) -> Result<(), CatalogContractError> {
    let expected = sources
        .iter()
        .flat_map(|source| {
            source
                .member_refs
                .iter()
                .copied()
                .map(|member_ref| (source.plan_source.clone(), member_ref))
        })
        .collect::<BTreeSet<_>>();
    let actual = bindings
        .iter()
        .map(|binding| (binding.source.clone(), binding.member_ref))
        .collect::<BTreeSet<_>>();
    if bindings.len() != expected.len() || actual != expected {
        return Err(CatalogContractError::invalid(
            "catalog publication requires exactly one session binding for every admitted member",
        ));
    }
    let session_assertions = reducer
        .sessions
        .iter()
        .map(|stored| (stored.fact.assertion_key, stored))
        .collect::<BTreeMap<_, _>>();
    let mut assertion_keys = BTreeSet::new();
    let mut member_sessions = BTreeMap::new();
    let mut bound_sessions = BTreeSet::new();
    let live = live_coverage_owners(sources)?;
    for binding in bindings {
        if !assertion_keys.insert(binding.assertion_key) {
            return Err(CatalogContractError::invalid(
                "one catalog session assertion cannot own multiple admitted members",
            ));
        }
        if member_sessions
            .insert(binding.member_ref, binding.session_ref)
            .is_some_and(|existing| existing != binding.session_ref)
        {
            return Err(CatalogContractError::invalid(
                "one catalog member identity cannot converge on different base sessions across sources",
            ));
        }
        let stored = session_assertions
            .get(&binding.assertion_key)
            .ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog member binding references an unknown live session assertion",
                )
            })?;
        if stored.fact.session_ref != binding.session_ref
            || stored.fact.owner.adapter_id != binding.source.adapter_id
            || stored.fact.owner.source_instance_key != binding.source.source_instance_key
            || !live.contains(&stored.fact.owner)
        {
            return Err(CatalogContractError::invalid(
                "catalog member binding does not match its exact live source-owned session assertion",
            ));
        }
        bound_sessions.insert(binding.session_ref);
    }
    let reducer_sessions = reducer
        .session_rows
        .iter()
        .map(|row| row.session_ref)
        .collect::<BTreeSet<_>>();
    if reducer_sessions != bound_sessions {
        return Err(CatalogContractError::invalid(
            "catalog publication cannot add or omit a live session outside admitted membership",
        ));
    }
    Ok(())
}

fn hash_component(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn hash_selection(hasher: &mut blake3::Hasher, selection: &ContractVersionSelection) {
    hasher.update(&selection.selection_contract_version.to_be_bytes());
    hasher.update(&selection.model_major.to_be_bytes());
    hasher.update(&selection.external_entity_reference_version.to_be_bytes());
    hasher.update(&selection.semantic_revision_reference_version.to_be_bytes());
    hasher.update(&selection.coverage_contract_version.to_be_bytes());
    hasher.update(&(selection.fact_family_versions.len() as u64).to_be_bytes());
    for (family, version) in &selection.fact_family_versions {
        hash_component(hasher, family.as_bytes());
        hasher.update(&version.to_be_bytes());
    }
    for version in [
        selection.query_pack_version,
        selection.observation_contract_version,
    ] {
        match version {
            Some(version) => {
                hasher.update(&[1]);
                hasher.update(&version.to_be_bytes());
            }
            None => {
                hasher.update(&[0]);
            }
        }
    }
}

fn hash_plan_source(hasher: &mut blake3::Hasher, source: &CatalogCoveragePlanSource) {
    hash_component(hasher, source.adapter_id.as_bytes());
    hasher.update(source.source_instance_key.as_bytes());
    hash_component(hasher, source.support_release_id.as_bytes());
    hasher.update(source.catalog_declaration_digest.as_bytes());
    hasher.update(source.access_policy_digest.as_bytes());
}

fn hash_coverage(
    hasher: &mut blake3::Hasher,
    coverage: &SourceCoverageSet,
) -> Result<(), CatalogContractError> {
    let encoded = serde_json::to_vec(coverage).map_err(|error| {
        CatalogContractError::invalid(format!(
            "catalog publication could not encode canonical source coverage: {error}"
        ))
    })?;
    hash_component(hasher, &encoded);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn derive_complete_source_digest(
    plan_source: &CatalogCoveragePlanSource,
    selection: &ContractVersionSelection,
    member_identity_contract_id: &str,
    membership_revision: CatalogSourceMembershipRevision,
    component_completion_revision: CatalogSourceCompletionRevision,
    member_refs: &[CatalogPublicationMemberRef],
    source_coverage: &SourceCoverageSet,
) -> Result<CatalogCompleteSourceDigest, CatalogContractError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-complete-source-v1\0");
    hash_plan_source(&mut hasher, plan_source);
    hash_selection(&mut hasher, selection);
    hash_component(&mut hasher, member_identity_contract_id.as_bytes());
    hasher.update(membership_revision.as_bytes());
    hasher.update(component_completion_revision.as_bytes());
    hasher.update(&(member_refs.len() as u64).to_be_bytes());
    for member_ref in member_refs {
        hasher.update(member_ref.as_bytes());
    }
    hash_coverage(&mut hasher, source_coverage)?;
    Ok(CatalogCompleteSourceDigest::from_digest(
        *hasher.finalize().as_bytes(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn derive_initial_publication_digest(
    build: CatalogInitialBuildExpectation,
    selection: &ContractVersionSelection,
    member_identity_contract_id: Option<&str>,
    sources: &[CatalogCompleteSourceAssembly],
    bindings: &[CatalogPublicationMemberBinding],
    reducer: &CatalogReducerPublication,
    limits: CatalogPublicationLimits,
) -> CatalogInitialPublicationDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-initial-publication-v1\0");
    hasher.update(&CATALOG_INITIAL_PUBLICATION_CONTRACT_VERSION.to_be_bytes());
    hasher.update(build.coverage_plan_id.storage_bytes());
    hasher.update(&build.desired_contract_version.to_be_bytes());
    hasher.update(&build.epoch.to_be_bytes());
    hasher.update(&build.attempt.to_be_bytes());
    hash_selection(&mut hasher, selection);
    match member_identity_contract_id {
        Some(value) => {
            hasher.update(&[1]);
            hash_component(&mut hasher, value.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&(limits.max_members as u64).to_be_bytes());
    hasher.update(&(limits.reducer.max_reducer_entries as u64).to_be_bytes());
    hasher.update(&(limits.reducer.max_rows as u64).to_be_bytes());
    hasher.update(&(sources.len() as u64).to_be_bytes());
    for source in sources {
        hasher.update(source.digest.as_bytes());
    }
    hasher.update(&(bindings.len() as u64).to_be_bytes());
    for binding in bindings {
        hash_plan_source(&mut hasher, &binding.source);
        hasher.update(binding.member_ref.as_bytes());
        hasher.update(binding.assertion_key.publication_bytes());
        hasher.update(
            &binding
                .session_ref
                .external_ref
                .external_entity_reference_version
                .to_be_bytes(),
        );
        hasher.update(binding.session_ref.external_ref.entity_key.as_bytes());
    }
    hasher.update(reducer.revision().as_bytes());
    hasher.update(&(reducer.project_row_count() as u64).to_be_bytes());
    hasher.update(&(reducer.session_row_count() as u64).to_be_bytes());
    hasher.update(&(reducer.tombstone_count() as u64).to_be_bytes());
    CatalogInitialPublicationDigest::from_digest(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests;
