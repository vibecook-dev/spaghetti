//! Source-neutral RFC 012B handoff from complete native membership to B3.
//!
//! A B2 producer proves a complete opaque member set and exact source-object
//! coverage. B3 additionally requires typed reducer assertions and a one-to-one
//! binding from every admitted member to its base session assertion. This
//! module is the only bridge between those contracts. It deliberately accepts
//! no durable row DTOs and derives semantic revisions from native semantic
//! values rather than source topology or observation coordinates.

use std::collections::BTreeSet;
use std::fmt;

use crate::adapter::{
    CanonicalEntityKey, CanonicalFactId, ContractCompleteness, CoverageAbsenceKind, FactRevisionId,
    NativeIdentity, QualifiedValue, QualifiedValueQuality, SemanticRevisionRef,
};
use crate::catalog_contract::evidence::{
    CatalogAvailability, CatalogDisclosureClass, CatalogEntityRef, CatalogEvidenceOwner,
    CatalogFieldAuthority, CatalogProjectAssertion, CatalogQualifiedField, CatalogReducer,
    CatalogReducerPublication, CatalogRetractionCause, CatalogRetractionEvidence,
    CatalogSessionAssertion, ProjectAssociationBasis, SessionProjectAssociationFact,
};
use crate::catalog_contract::publication::{
    CatalogCompleteSourceAssembly, CatalogInitialPublicationAssembly, CatalogPublicationLimits,
    CatalogPublicationMemberBinding, CatalogPublicationMemberHistory, CatalogPublicationMemberRef,
    CatalogRefreshPredecessor, CatalogRefreshPublicationAssembly,
};
use crate::catalog_contract::{
    CatalogCoveragePlan, CatalogCoverageScope, CatalogReadinessSnapshot,
};

use super::catalog_composition::{
    CatalogCompositionError, CatalogMemberRef, CatalogRefreshCoverageGenerations,
};

const PROJECTION_REVISION_CONTRACT_VERSION: u32 = 1;
const NATIVE_IDENTITY_AUTHORITY: &str = "native-catalog-identity";
const AVAILABILITY_AUTHORITY: &str = "catalog-membership-availability";
const ASSOCIATION_AUTHORITY: &str = "native-project-association";

/// One admitted native session and the exact covered object that owns its
/// minimum catalog evidence. Fields stay private so callers cannot bypass the
/// canonical member/reference checks performed by [`CatalogSourceProjection`].
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogSourceMemberProjection {
    owner: CatalogEvidenceOwner,
    native_project_key: String,
    native_session_id: String,
    availability: CatalogAvailability,
    association_basis: ProjectAssociationBasis,
}

impl CatalogSourceMemberProjection {
    pub(crate) fn new(
        owner: CatalogEvidenceOwner,
        native_project_key: impl Into<String>,
        native_session_id: impl Into<String>,
        availability: CatalogAvailability,
        association_basis: ProjectAssociationBasis,
    ) -> Self {
        Self {
            owner,
            native_project_key: native_project_key.into(),
            native_session_id: native_session_id.into(),
            availability,
            association_basis,
        }
    }

    pub(crate) fn reconcile_refresh_generation(
        &mut self,
        generations: &CatalogRefreshCoverageGenerations,
    ) -> Result<(), CatalogCompositionError> {
        self.owner = generations.reconcile_owner(&self.owner)?;
        Ok(())
    }
}

impl fmt::Debug for CatalogSourceMemberProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogSourceMemberProjection")
            .field("adapter_id", &self.owner.adapter_id)
            .field("source_instance_key", &self.owner.source_instance_key)
            .field("generation", &self.owner.generation)
            .field("availability", &availability_tag(&self.availability))
            .field("association_basis", &self.association_basis)
            .finish_non_exhaustive()
    }
}

/// Checked B2-to-B3 projection for one complete source. Applying it to a
/// reducer is atomic in memory: a rejected assertion cannot leave a prefix of
/// the source installed.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogSourceProjection {
    source: CatalogCompleteSourceAssembly,
    project_assertions: Vec<CatalogProjectAssertion>,
    session_assertions: Vec<CatalogSessionAssertion>,
    associations: Vec<SessionProjectAssociationFact>,
    member_bindings: Vec<CatalogPublicationMemberBinding>,
}

impl CatalogSourceProjection {
    pub(crate) fn assemble(
        source: CatalogCompleteSourceAssembly,
        mut members: Vec<CatalogSourceMemberProjection>,
    ) -> Result<Self, CatalogCompositionError> {
        if members.len() != source.member_count() {
            return Err(CatalogCompositionError::invalid(
                "catalog source projection requires exactly one input for every admitted member",
            ));
        }

        let plan_source = source.plan_source();
        let live_owners = source
            .source_coverage()
            .points
            .iter()
            .map(|point| {
                CatalogEvidenceOwner::new(
                    point.adapter_id.clone(),
                    point.source_instance_key,
                    point.stream_key,
                    point.object_key,
                    point.generation,
                )
                .map_err(|_| {
                    CatalogCompositionError::invalid(
                        "catalog source projection contains invalid coverage ownership",
                    )
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;

        members.sort_by(|left, right| {
            left.native_session_id
                .cmp(&right.native_session_id)
                .then_with(|| left.native_project_key.cmp(&right.native_project_key))
                .then_with(|| left.owner.cmp(&right.owner))
        });

        let mut seen_members = BTreeSet::new();
        let mut project_assertions = Vec::with_capacity(members.len());
        let mut session_assertions = Vec::with_capacity(members.len());
        let mut associations = Vec::with_capacity(members.len());
        let mut member_bindings = Vec::with_capacity(members.len());

        for member in members {
            if member.owner.adapter_id != plan_source.adapter_id
                || member.owner.source_instance_key != plan_source.source_instance_key
                || !live_owners.contains(&member.owner)
            {
                return Err(CatalogCompositionError::invalid(
                    "catalog source projection evidence is outside complete live coverage",
                ));
            }

            let member_ref = CatalogMemberRef::from_canonical_session(
                source.member_identity_contract_id(),
                &plan_source.adapter_id,
                plan_source.source_instance_key,
                member.native_session_id.as_bytes(),
            )?;
            if !seen_members.insert(member_ref) {
                return Err(CatalogCompositionError::invalid(
                    "catalog source projection contains duplicate canonical members",
                ));
            }
            let publication_member_ref =
                CatalogPublicationMemberRef::from_digest(*member_ref.as_bytes());

            let project_key = CanonicalEntityKey::derive(
                &plan_source.adapter_id,
                &plan_source.source_instance_key,
                "project",
                member.native_project_key.as_bytes(),
            )
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "catalog source projection contains an invalid native project identity",
                )
            })?;
            let session_key = CanonicalEntityKey::derive(
                &plan_source.adapter_id,
                &plan_source.source_instance_key,
                "session",
                member.native_session_id.as_bytes(),
            )
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "catalog source projection contains an invalid native session identity",
                )
            })?;
            let project_ref = CatalogEntityRef::project(project_key);
            let session_ref = CatalogEntityRef::session(session_key);
            let availability_semantics = availability_semantics(&member.availability);

            let project_revision = semantic_revision(
                &member.owner,
                "catalog.project",
                member.native_project_key.as_bytes(),
                &[
                    member.native_project_key.as_bytes(),
                    &availability_semantics,
                ],
            )?;
            let session_revision = semantic_revision(
                &member.owner,
                "catalog.session",
                member.native_session_id.as_bytes(),
                &[
                    member.native_session_id.as_bytes(),
                    member.native_project_key.as_bytes(),
                    &availability_semantics,
                ],
            )?;
            let association_revision = semantic_revision(
                &member.owner,
                "catalog.project-association",
                member.native_session_id.as_bytes(),
                &[
                    member.native_session_id.as_bytes(),
                    member.native_project_key.as_bytes(),
                    association_basis_tag(member.association_basis),
                ],
            )?;

            let project_identity = known_field(
                NativeIdentity {
                    native_namespace: "catalog.project".to_owned(),
                    native_id: member.native_project_key.clone(),
                },
                QualifiedValueQuality::NativeClaimed,
                NATIVE_IDENTITY_AUTHORITY,
                CatalogDisclosureClass::LocalSensitive,
                project_revision,
            )?;
            let project_availability = known_field(
                member.availability.clone(),
                QualifiedValueQuality::Exact,
                AVAILABILITY_AUTHORITY,
                CatalogDisclosureClass::Public,
                project_revision,
            )?;
            let project_stable_key = projection_key(
                b"project-assertion",
                member_ref.as_bytes(),
                member.native_project_key.as_bytes(),
            );
            let project_assertion = CatalogProjectAssertion::new(
                member.owner.clone(),
                &project_stable_key,
                project_ref,
                Some(project_identity),
                None,
                None,
                None,
                None,
                project_availability,
                vec![project_revision],
            )
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "catalog project evidence is outside the bounded projection contract",
                )
            })?;

            let session_identity = known_field(
                NativeIdentity {
                    native_namespace: "catalog.session".to_owned(),
                    native_id: member.native_session_id.clone(),
                },
                QualifiedValueQuality::NativeClaimed,
                NATIVE_IDENTITY_AUTHORITY,
                CatalogDisclosureClass::LocalSensitive,
                session_revision,
            )?;
            let session_availability = known_field(
                member.availability,
                QualifiedValueQuality::Exact,
                AVAILABILITY_AUTHORITY,
                CatalogDisclosureClass::Public,
                session_revision,
            )?;
            let session_stable_key = projection_key(
                b"session-assertion",
                member_ref.as_bytes(),
                member.native_session_id.as_bytes(),
            );
            let session_assertion = CatalogSessionAssertion::new(
                member.owner.clone(),
                &session_stable_key,
                session_ref,
                Some(session_identity),
                None,
                None,
                None,
                None,
                None,
                None,
                session_availability,
                vec![session_revision],
            )
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "catalog session evidence is outside the bounded projection contract",
                )
            })?;
            let association_stable_key = association_projection_key(
                member_ref.as_bytes(),
                member.native_session_id.as_bytes(),
                member.native_project_key.as_bytes(),
            );
            let association = SessionProjectAssociationFact::new(
                member.owner,
                &association_stable_key,
                session_ref,
                project_ref,
                member.association_basis,
                None,
                None,
                CatalogFieldAuthority::new(ASSOCIATION_AUTHORITY, 100, true).map_err(|_| {
                    CatalogCompositionError::invalid("catalog association authority is invalid")
                })?,
                QualifiedValueQuality::NativeClaimed,
                ContractCompleteness::Complete,
                None,
                vec![association_revision],
            )
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "catalog association evidence is outside the bounded projection contract",
                )
            })?;
            let binding = source
                .member_binding(
                    publication_member_ref,
                    session_assertion.assertion_key,
                    session_ref,
                )
                .map_err(|_| {
                    CatalogCompositionError::invalid(
                        "catalog source projection does not match complete membership",
                    )
                })?;

            project_assertions.push(project_assertion);
            session_assertions.push(session_assertion);
            associations.push(association);
            member_bindings.push(binding);
        }

        Ok(Self {
            source,
            project_assertions,
            session_assertions,
            associations,
            member_bindings,
        })
    }

    pub(crate) fn reduce_into(
        &self,
        mut reducer: CatalogReducer,
        observation_commit: u64,
    ) -> Result<CatalogReducer, CatalogCompositionError> {
        for assertion in &self.project_assertions {
            reducer
                .upsert_project_assertion(assertion.clone(), observation_commit)
                .map_err(|_| {
                    CatalogCompositionError::invalid(
                        "catalog project projection could not be reduced",
                    )
                })?;
        }
        for assertion in &self.session_assertions {
            reducer
                .upsert_session_assertion(assertion.clone(), observation_commit)
                .map_err(|_| {
                    CatalogCompositionError::invalid(
                        "catalog session projection could not be reduced",
                    )
                })?;
        }
        for association in &self.associations {
            reducer
                .upsert_association(association.clone(), observation_commit)
                .map_err(|_| {
                    CatalogCompositionError::invalid(
                        "catalog association projection could not be reduced",
                    )
                })?;
        }
        Ok(reducer)
    }

    pub(crate) fn member_count(&self) -> usize {
        self.member_bindings.len()
    }

    pub(crate) fn into_publication_parts(
        self,
    ) -> (
        CatalogCompleteSourceAssembly,
        Vec<CatalogPublicationMemberBinding>,
    ) {
        (self.source, self.member_bindings)
    }
}

impl fmt::Debug for CatalogSourceProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogSourceProjection")
            .field("source", &self.source)
            .field("project_assertions", &self.project_assertions.len())
            .field("session_assertions", &self.session_assertions.len())
            .field("associations", &self.associations.len())
            .field("member_bindings", &self.member_bindings.len())
            .finish()
    }
}

/// Canonical all-source input to one initial Library publication. The batch
/// owns its reducer candidate and complete source bindings; no caller can pass
/// independently assembled rows, coverage, or member bindings to B3.
pub(crate) struct CatalogInitialProjectionBatch {
    plan: CatalogCoveragePlan,
    contract_selection: crate::adapter::ContractVersionSelection,
    reducer: CatalogReducer,
    sources: Vec<CatalogCompleteSourceAssembly>,
    member_bindings: Vec<CatalogPublicationMemberBinding>,
}

impl CatalogInitialProjectionBatch {
    pub(crate) fn assemble(
        projections: Vec<CatalogSourceProjection>,
        contract_selection: crate::adapter::ContractVersionSelection,
        observation_commit: u64,
    ) -> Result<Self, CatalogCompositionError> {
        if observation_commit == 0 {
            return Err(CatalogCompositionError::invalid(
                "catalog initial projection requires a positive observation commit",
            ));
        }
        let mut reducer = CatalogReducer::default();
        let mut sources = Vec::with_capacity(projections.len());
        let mut member_bindings = Vec::new();
        for projection in projections {
            reducer = projection.reduce_into(reducer, observation_commit)?;
            let (source, mut bindings) = projection.into_publication_parts();
            sources.push(source);
            member_bindings.append(&mut bindings);
        }
        let plan = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            sources
                .iter()
                .map(|source| source.plan_source().clone())
                .collect(),
            Vec::new(),
        )
        .map_err(|_| {
            CatalogCompositionError::invalid(
                "catalog initial projection sources do not form one canonical Library plan",
            )
        })?;
        Ok(Self {
            plan,
            contract_selection,
            reducer,
            sources,
            member_bindings,
        })
    }

    pub(crate) fn plan(&self) -> &CatalogCoveragePlan {
        &self.plan
    }

    pub(crate) fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub(crate) fn member_count(&self) -> usize {
        self.member_bindings.len()
    }

    pub(crate) fn into_publication(
        self,
        readiness: &CatalogReadinessSnapshot,
        limits: CatalogPublicationLimits,
    ) -> Result<CatalogInitialPublicationAssembly, CatalogCompositionError> {
        CatalogInitialPublicationAssembly::assemble(
            &self.plan,
            readiness,
            self.contract_selection,
            self.sources,
            &self.reducer,
            self.member_bindings,
            limits,
        )
        .map_err(|_| {
            CatalogCompositionError::invalid(
                "catalog initial projection does not match the frozen build lineage",
            )
        })
    }
}

impl fmt::Debug for CatalogInitialProjectionBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogInitialProjectionBatch")
            .field("coverage_plan_id", &self.plan.coverage_plan_id)
            .field("source_count", &self.sources.len())
            .field("member_count", &self.member_bindings.len())
            .finish()
    }
}

/// Canonical all-source successor input for one ordinary Library refresh. It
/// resumes the authenticated predecessor reducer, applies current complete
/// projections, and retracts only prior owner generations named by explicit
/// complete-coverage absence/replacement evidence.
pub(crate) struct CatalogRefreshProjectionBatch {
    plan: CatalogCoveragePlan,
    contract_selection: crate::adapter::ContractVersionSelection,
    reducer: CatalogReducer,
    sources: Vec<CatalogCompleteSourceAssembly>,
    member_bindings: Vec<CatalogPublicationMemberBinding>,
}

impl CatalogRefreshProjectionBatch {
    pub(crate) fn assemble(
        projections: Vec<CatalogSourceProjection>,
        contract_selection: crate::adapter::ContractVersionSelection,
        observation_commit: u64,
        prior_reducer: &CatalogReducerPublication,
    ) -> Result<Self, CatalogCompositionError> {
        if observation_commit == 0 {
            return Err(CatalogCompositionError::invalid(
                "catalog refresh projection requires a positive observation commit",
            ));
        }
        let absence_commit = observation_commit.checked_add(1).ok_or_else(|| {
            CatalogCompositionError::invalid("catalog refresh observation commit overflowed")
        })?;
        let plan = CatalogCoveragePlan::new(
            CatalogCoverageScope::Library,
            projections
                .iter()
                .map(|projection| projection.source.plan_source().clone())
                .collect(),
            Vec::new(),
        )
        .map_err(|_| {
            CatalogCompositionError::invalid(
                "catalog refresh projection sources do not form one canonical Library plan",
            )
        })?;

        let mut reducer = prior_reducer.resume_for_refresh();
        for projection in &projections {
            reducer = projection.reduce_into(reducer, observation_commit)?;
        }
        for owner in prior_reducer.live_owners() {
            let Some(projection) = projections.iter().find(|projection| {
                let source = projection.source.plan_source();
                source.adapter_id == owner.adapter_id
                    && source.source_instance_key == owner.source_instance_key
            }) else {
                return Err(CatalogCompositionError::invalid(
                    "catalog refresh predecessor owner has no matching complete source",
                ));
            };
            let coverage = projection.source.source_coverage();
            let Some(absence) = coverage
                .explicit_absence_or_deletion
                .iter()
                .find(|absence| {
                    absence.stream_key == owner.stream_key
                        && absence.object_key == owner.object_key
                        && absence.generation == owner.generation
                })
            else {
                continue;
            };
            let replacement = coverage.points.iter().any(|point| {
                point.stream_key == owner.stream_key
                    && point.object_key == owner.object_key
                    && point.generation > owner.generation
            });
            let cause = if replacement {
                CatalogRetractionCause::ConfirmedReplacement
            } else {
                match absence.kind {
                    CoverageAbsenceKind::Absent | CoverageAbsenceKind::Deleted => {
                        CatalogRetractionCause::ConfirmedDeletion
                    }
                }
            };
            let evidence = CatalogRetractionEvidence::new(
                owner.clone(),
                cause,
                ContractCompleteness::Complete,
                vec![refresh_retraction_revision(
                    &owner,
                    coverage.membership_revision.as_bytes(),
                    cause,
                    replacement.then(|| {
                        coverage
                            .points
                            .iter()
                            .filter(|point| {
                                point.stream_key == owner.stream_key
                                    && point.object_key == owner.object_key
                            })
                            .map(|point| point.generation)
                            .max()
                            .expect("replacement point checked above")
                    }),
                )?],
            )
            .map_err(|_| {
                CatalogCompositionError::invalid(
                    "catalog refresh retraction evidence is outside contract bounds",
                )
            })?;
            let retraction = reducer
                .retract_owner(&evidence, observation_commit)
                .map_err(|_| {
                    CatalogCompositionError::invalid(
                        "catalog refresh could not retract replaced source evidence",
                    )
                })?;
            for entity_ref in retraction.orphaned_entities {
                reducer
                    .confirm_absent(entity_ref, &evidence, absence_commit)
                    .map_err(|_| {
                        CatalogCompositionError::invalid(
                            "catalog refresh could not confirm an orphaned entity absence",
                        )
                    })?;
            }
        }

        let mut sources = Vec::with_capacity(projections.len());
        let mut member_bindings = Vec::new();
        for projection in projections {
            let (source, mut bindings) = projection.into_publication_parts();
            sources.push(source);
            member_bindings.append(&mut bindings);
        }
        Ok(Self {
            plan,
            contract_selection,
            reducer,
            sources,
            member_bindings,
        })
    }

    pub(crate) fn plan(&self) -> &CatalogCoveragePlan {
        &self.plan
    }

    pub(crate) fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub(crate) fn member_count(&self) -> usize {
        self.member_bindings.len()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn into_publication(
        self,
        readiness: &CatalogReadinessSnapshot,
        refresh_started_commit_seq: u64,
        predecessor: CatalogRefreshPredecessor,
        prior_reducer: &CatalogReducerPublication,
        prior_member_history: &CatalogPublicationMemberHistory,
        limits: CatalogPublicationLimits,
    ) -> Result<CatalogRefreshPublicationAssembly, CatalogCompositionError> {
        CatalogRefreshPublicationAssembly::assemble(
            &self.plan,
            readiness,
            refresh_started_commit_seq,
            predecessor,
            prior_reducer,
            prior_member_history,
            self.contract_selection,
            self.sources,
            &self.reducer,
            self.member_bindings,
            limits,
        )
        .map_err(|_| {
            CatalogCompositionError::invalid(
                "catalog refresh projection does not match the frozen refresh lineage",
            )
        })
    }
}

impl fmt::Debug for CatalogRefreshProjectionBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogRefreshProjectionBatch")
            .field("coverage_plan_id", &self.plan.coverage_plan_id)
            .field("source_count", &self.sources.len())
            .field("member_count", &self.member_bindings.len())
            .finish()
    }
}

fn refresh_retraction_revision(
    owner: &CatalogEvidenceOwner,
    membership_revision: &[u8; 32],
    cause: CatalogRetractionCause,
    replacement_generation: Option<u64>,
) -> Result<SemanticRevisionRef, CatalogCompositionError> {
    let mut key_hasher = blake3::Hasher::new();
    key_hasher.update(b"spaghetti/rfc012b/catalog-refresh-retraction-key-v1\0");
    key_hasher.update(owner.stream_key.as_bytes());
    key_hasher.update(owner.object_key.as_bytes());
    key_hasher.update(&owner.generation.to_be_bytes());
    let fact_id = CanonicalFactId::native(
        &owner.adapter_id,
        &owner.source_instance_key,
        "catalog.owner-retraction",
        key_hasher.finalize().as_bytes(),
    )
    .map_err(|_| CatalogCompositionError::invalid("catalog retraction identity is invalid"))?;
    let mut semantic = blake3::Hasher::new();
    semantic.update(b"spaghetti/rfc012b/catalog-refresh-retraction-v1\0");
    semantic.update(membership_revision);
    semantic.update(&[match cause {
        CatalogRetractionCause::ConfirmedDeletion => 1,
        CatalogRetractionCause::ConfirmedReplacement => 2,
        CatalogRetractionCause::TemporarilyUnavailable => 3,
    }]);
    semantic.update(&replacement_generation.unwrap_or(0).to_be_bytes());
    let revision = FactRevisionId::derive(
        &fact_id,
        PROJECTION_REVISION_CONTRACT_VERSION,
        semantic.finalize().as_bytes(),
    )
    .map_err(|_| CatalogCompositionError::invalid("catalog retraction revision is invalid"))?;
    Ok(SemanticRevisionRef::new(revision))
}

fn known_field<T>(
    value: T,
    quality: QualifiedValueQuality,
    authority: &str,
    disclosure: CatalogDisclosureClass,
    revision: SemanticRevisionRef,
) -> Result<CatalogQualifiedField<T>, CatalogCompositionError> {
    let authority = CatalogFieldAuthority::new(authority, 100, true)
        .map_err(|_| CatalogCompositionError::invalid("catalog field authority is invalid"))?;
    let qualified = QualifiedValue::from_parts(
        Some(value),
        quality,
        authority,
        ContractCompleteness::Complete,
        None,
        None,
        vec![revision],
    )
    .map_err(|_| CatalogCompositionError::invalid("catalog qualified field is invalid"))?;
    CatalogQualifiedField::new(qualified, disclosure)
        .map_err(|_| CatalogCompositionError::invalid("catalog qualified field is invalid"))
}

fn semantic_revision(
    owner: &CatalogEvidenceOwner,
    fact_kind: &str,
    stable_native_fact_key: &[u8],
    semantic_components: &[&[u8]],
) -> Result<SemanticRevisionRef, CatalogCompositionError> {
    let fact_id = CanonicalFactId::native(
        &owner.adapter_id,
        &owner.source_instance_key,
        fact_kind,
        stable_native_fact_key,
    )
    .map_err(|_| CatalogCompositionError::invalid("catalog semantic fact identity is invalid"))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-semantic-revision-v1\0");
    hasher.update(&(semantic_components.len() as u64).to_be_bytes());
    for component in semantic_components {
        hasher.update(&(component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    let digest = hasher.finalize();
    let revision = FactRevisionId::derive(
        &fact_id,
        PROJECTION_REVISION_CONTRACT_VERSION,
        digest.as_bytes(),
    )
    .map_err(|_| CatalogCompositionError::invalid("catalog semantic revision is invalid"))?;
    Ok(SemanticRevisionRef::new(revision))
}

fn projection_key(domain: &[u8], member_ref: &[u8; 32], native_key: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-source-projection-key-v1\0");
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    hasher.update(member_ref);
    hasher.update(&(native_key.len() as u64).to_be_bytes());
    hasher.update(native_key);
    *hasher.finalize().as_bytes()
}

fn association_projection_key(
    member_ref: &[u8; 32],
    native_session_id: &[u8],
    native_project_key: &[u8],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-source-projection-key-v1\0");
    let domain = b"project-association";
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    hasher.update(member_ref);
    hasher.update(&(native_session_id.len() as u64).to_be_bytes());
    hasher.update(native_session_id);
    hasher.update(&(native_project_key.len() as u64).to_be_bytes());
    hasher.update(native_project_key);
    *hasher.finalize().as_bytes()
}

fn availability_tag(availability: &CatalogAvailability) -> &'static str {
    match availability {
        CatalogAvailability::MetadataOnly => "metadata_only",
        CatalogAvailability::TranscriptDiscovered => "transcript_discovered",
        CatalogAvailability::Hydrating => "hydrating",
        CatalogAvailability::HistoryReady => "history_ready",
        CatalogAvailability::Unavailable { .. } => "unavailable",
    }
}

fn availability_semantics(availability: &CatalogAvailability) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-availability-v1\0");
    let tag = availability_tag(availability).as_bytes();
    hasher.update(&(tag.len() as u64).to_be_bytes());
    hasher.update(tag);
    if let CatalogAvailability::Unavailable { reason } = availability {
        hasher.update(&(reason.len() as u64).to_be_bytes());
        hasher.update(reason.as_bytes());
    } else {
        hasher.update(&0_u64.to_be_bytes());
    }
    *hasher.finalize().as_bytes()
}

fn association_basis_tag(basis: ProjectAssociationBasis) -> &'static [u8] {
    match basis {
        ProjectAssociationBasis::NativeProjectIndex => b"native_project_index",
        ProjectAssociationBasis::TranscriptCwd => b"transcript_cwd",
        ProjectAssociationBasis::SessionDirectory => b"session_directory",
        ProjectAssociationBasis::RolloutHeader => b"rollout_header",
        ProjectAssociationBasis::DeclaredDerivedAncestor => b"declared_derived_ancestor",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::adapter::{
        CanonicalSourceInstanceKey, ContractVersionSelection, CoverageAbsence, CoverageAbsenceKind,
        CoverageDeclarationDigest, CoverageDomain, CoverageMembershipRevision, CoverageObjectKey,
        CoveragePosition, CoveragePositionKind, CoverageProvenance, CoverageSetCompleteness,
        CoverageStatus, CoverageStreamKey, SourceCoveragePoint, SourceCoverageSet,
        CONTRACT_VERSION_SELECTION_VERSION,
    };
    use crate::catalog_contract::evidence::{
        CatalogAssociationCoverage, CatalogReducerPublicationLimits,
    };
    use crate::catalog_contract::publication::{
        CatalogSourceCompletionRevision, CatalogSourceMembershipRevision,
    };
    use crate::catalog_contract::{
        CatalogAccessPolicyDigest, CatalogCoveragePlanSource, CatalogCoverageScope,
        CatalogReadinessMachine, CATALOG_PROJECTION_PACK_ID, CATALOG_QUERY_PACK_CONTRACT_VERSION,
    };

    const ADAPTER_ID: &str = "fixture-agent";
    const MEMBER_CONTRACT: &str = "catalog-session-identity-v1";

    fn selection() -> ContractVersionSelection {
        ContractVersionSelection {
            selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
            model_major: 1,
            external_entity_reference_version: 1,
            semantic_revision_reference_version: 1,
            coverage_contract_version: 1,
            fact_family_versions: BTreeMap::from([
                ("catalog.project".to_owned(), 1),
                ("catalog.session".to_owned(), 1),
            ]),
            query_pack_version: Some(1),
            observation_contract_version: None,
        }
    }

    fn fixture(
        object_label: &str,
    ) -> (CatalogCompleteSourceAssembly, CatalogSourceMemberProjection) {
        fixture_at_generation(object_label, 1)
    }

    fn fixture_at_generation(
        object_label: &str,
        generation: u64,
    ) -> (CatalogCompleteSourceAssembly, CatalogSourceMemberProjection) {
        let source_key = CanonicalSourceInstanceKey::derive(1, b"projection-source").unwrap();
        let stream_key = CoverageStreamKey::derive(ADAPTER_ID, b"catalog-stream").unwrap();
        let object_key =
            CoverageObjectKey::derive("catalog-stream", object_label.as_bytes()).unwrap();
        let owner =
            CatalogEvidenceOwner::new(ADAPTER_ID, source_key, stream_key, object_key, generation)
                .unwrap();
        let plan_source = CatalogCoveragePlanSource::new(
            ADAPTER_ID,
            source_key,
            "fixture-support-v1",
            CoverageDeclarationDigest::derive(b"fixture-declaration-v1").unwrap(),
            CatalogAccessPolicyDigest::derive(1, b"fixture-withheld-policy-v1").unwrap(),
        )
        .unwrap();
        let domain = CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
            version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
        };
        let point = SourceCoveragePoint::new(
            domain.clone(),
            ADAPTER_ID,
            source_key,
            stream_key,
            object_key,
            generation,
            Some(
                CoveragePosition::derive(
                    CoveragePositionKind::SnapshotRevision,
                    object_label.as_bytes(),
                    None,
                )
                .unwrap(),
            ),
            CoverageStatus::ExactSnapshot,
            CoverageProvenance::default(),
        )
        .unwrap();
        let coverage = SourceCoverageSet::new(
            domain,
            plan_source.coverage_scope(CatalogCoverageScope::Library),
            CoverageMembershipRevision::derive(object_label.as_bytes()).unwrap(),
            vec![point],
            (generation > 1)
                .then(|| CoverageAbsence {
                    stream_key,
                    object_key,
                    generation: generation - 1,
                    kind: CoverageAbsenceKind::Deleted,
                })
                .into_iter()
                .collect(),
            Vec::new(),
            CoverageSetCompleteness::Complete,
        )
        .unwrap();
        let member_ref = CatalogMemberRef::from_canonical_session(
            MEMBER_CONTRACT,
            ADAPTER_ID,
            source_key,
            b"session-a",
        )
        .unwrap();
        let source = CatalogCompleteSourceAssembly::from_complete_library_coverage(
            plan_source,
            selection(),
            MEMBER_CONTRACT,
            CatalogSourceMembershipRevision::from_digest(
                *blake3::hash(format!("membership:{object_label}:{generation}").as_bytes())
                    .as_bytes(),
            ),
            CatalogSourceCompletionRevision::from_digest(
                *blake3::hash(format!("completion:{object_label}:{generation}").as_bytes())
                    .as_bytes(),
            ),
            vec![CatalogPublicationMemberRef::from_digest(
                *member_ref.as_bytes(),
            )],
            coverage,
        )
        .unwrap();
        let input = CatalogSourceMemberProjection::new(
            owner,
            "project-a",
            "session-a",
            CatalogAvailability::TranscriptDiscovered,
            ProjectAssociationBasis::RolloutHeader,
        );
        (source, input)
    }

    fn deleted_fixture(object_label: &str) -> CatalogSourceProjection {
        let source_key = CanonicalSourceInstanceKey::derive(1, b"projection-source").unwrap();
        let stream_key = CoverageStreamKey::derive(ADAPTER_ID, b"catalog-stream").unwrap();
        let object_key =
            CoverageObjectKey::derive("catalog-stream", object_label.as_bytes()).unwrap();
        let plan_source = CatalogCoveragePlanSource::new(
            ADAPTER_ID,
            source_key,
            "fixture-support-v1",
            CoverageDeclarationDigest::derive(b"fixture-declaration-v1").unwrap(),
            CatalogAccessPolicyDigest::derive(1, b"fixture-withheld-policy-v1").unwrap(),
        )
        .unwrap();
        let domain = CoverageDomain::ProjectionPack {
            pack: CATALOG_PROJECTION_PACK_ID.to_owned(),
            version: CATALOG_QUERY_PACK_CONTRACT_VERSION,
        };
        let coverage = SourceCoverageSet::new(
            domain,
            plan_source.coverage_scope(CatalogCoverageScope::Library),
            CoverageMembershipRevision::derive(b"deleted-membership").unwrap(),
            Vec::new(),
            vec![CoverageAbsence {
                stream_key,
                object_key,
                generation: 1,
                kind: CoverageAbsenceKind::Deleted,
            }],
            Vec::new(),
            CoverageSetCompleteness::Complete,
        )
        .unwrap();
        let source = CatalogCompleteSourceAssembly::from_complete_library_coverage(
            plan_source,
            selection(),
            MEMBER_CONTRACT,
            CatalogSourceMembershipRevision::from_digest(
                *blake3::hash(b"deleted-membership").as_bytes(),
            ),
            CatalogSourceCompletionRevision::from_digest(
                *blake3::hash(b"deleted-completion").as_bytes(),
            ),
            Vec::new(),
            coverage,
        )
        .unwrap();
        CatalogSourceProjection::assemble(source, Vec::new()).unwrap()
    }

    #[test]
    fn complete_membership_projects_to_bounded_reducer_and_exact_binding() {
        let (source, input) = fixture("object-a");
        let projection = CatalogSourceProjection::assemble(source, vec![input]).unwrap();
        assert_eq!(projection.member_count(), 1);
        let debug = format!("{projection:?}");
        assert!(!debug.contains("project-a"));
        assert!(!debug.contains("session-a"));

        let reducer = projection
            .reduce_into(CatalogReducer::default(), 1)
            .unwrap();
        let frozen = reducer
            .freeze_for_initial_publication(CatalogReducerPublicationLimits::default())
            .unwrap();
        assert_eq!(frozen.project_row_count(), 1);
        assert_eq!(frozen.session_row_count(), 1);
        let (source, bindings) = projection.into_publication_parts();
        assert_eq!(source.member_count(), 1);
        assert_eq!(bindings.len(), 1);
    }

    #[test]
    fn initial_batch_freezes_plan_reducer_coverage_and_bindings_together() {
        let (source, input) = fixture("object-a");
        let projection = CatalogSourceProjection::assemble(source, vec![input]).unwrap();
        let batch =
            CatalogInitialProjectionBatch::assemble(vec![projection], selection(), 7).unwrap();
        assert_eq!(batch.source_count(), 1);
        assert_eq!(batch.member_count(), 1);
        let mut readiness = CatalogReadinessMachine::register(batch.plan().clone(), 1).unwrap();
        readiness.schedule_build().unwrap();
        let publication = batch
            .into_publication(readiness.snapshot(), CatalogPublicationLimits::default())
            .unwrap();
        assert_eq!(publication.source_count(), 1);
        assert_eq!(publication.member_count(), 1);
        assert_eq!(publication.project_row_count(), 1);
        assert_eq!(publication.session_row_count(), 1);
        let debug = format!("{publication:?}");
        assert!(!debug.contains("project-a"));
        assert!(!debug.contains("session-a"));
    }

    #[test]
    fn projection_rejects_uncovered_or_duplicate_members_without_echoing_values() {
        let (source, input) = fixture("object-a");
        let mut uncovered = input.clone();
        uncovered.owner = CatalogEvidenceOwner::new(
            ADAPTER_ID,
            uncovered.owner.source_instance_key,
            uncovered.owner.stream_key,
            CoverageObjectKey::derive("catalog-stream", b"private/path/session-a").unwrap(),
            1,
        )
        .unwrap();
        let error = CatalogSourceProjection::assemble(source.clone(), vec![uncovered])
            .unwrap_err()
            .to_string();
        assert!(!error.contains("private"));
        assert!(!error.contains("session-a"));

        let error = CatalogSourceProjection::assemble(source, vec![input.clone(), input])
            .unwrap_err()
            .to_string();
        assert!(error.contains("exactly one input"));
        assert!(!error.contains("session-a"));
    }

    #[test]
    fn semantic_revisions_ignore_equivalent_source_topology() {
        let (left_source, left_input) = fixture("object-a");
        let (right_source, right_input) = fixture("object-b");
        let left = CatalogSourceProjection::assemble(left_source, vec![left_input]).unwrap();
        let right = CatalogSourceProjection::assemble(right_source, vec![right_input]).unwrap();

        assert_eq!(
            left.project_assertions[0].provenance,
            right.project_assertions[0].provenance
        );
        assert_eq!(
            left.session_assertions[0].provenance,
            right.session_assertions[0].provenance
        );
        assert_eq!(
            left.associations[0].provenance,
            right.associations[0].provenance
        );
        assert_ne!(
            left.session_assertions[0].assertion_key, right.session_assertions[0].assertion_key,
            "source evidence identity must still distinguish topology"
        );
    }

    #[test]
    fn project_reassociation_retains_competing_evidence_and_selects_the_new_observation() {
        let (source, input) = fixture("object-a");
        let mut moved = input.clone();
        moved.native_project_key = "project-b".to_owned();
        let initial = CatalogSourceProjection::assemble(source.clone(), vec![input]).unwrap();
        let reassociated = CatalogSourceProjection::assemble(source, vec![moved]).unwrap();
        assert_ne!(
            initial.associations[0].association_key,
            reassociated.associations[0].association_key
        );

        let reducer = initial.reduce_into(CatalogReducer::default(), 1).unwrap();
        let reducer = reassociated.reduce_into(reducer, 2).unwrap();
        let session_ref = reassociated.session_assertions[0].session_ref;
        match reducer.association_for_session(session_ref) {
            CatalogAssociationCoverage::Available { selection } => {
                assert_eq!(
                    selection.association.project_ref,
                    reassociated.associations[0].project_ref
                );
                assert_eq!(selection.competing_associations.len(), 1);
                assert_eq!(selection.conflicting_association_keys.len(), 1);
            }
            CatalogAssociationCoverage::Unknown => panic!("reassociation must remain available"),
        }
    }

    #[test]
    fn refresh_batch_replaces_and_tombstones_only_explicitly_absent_owner_generations() {
        let (initial_source, initial_member) = fixture("object-a");
        let initial =
            CatalogSourceProjection::assemble(initial_source, vec![initial_member]).unwrap();
        let initial_reducer = initial.reduce_into(CatalogReducer::default(), 1).unwrap();
        let prior = initial_reducer
            .freeze_for_initial_publication(CatalogReducerPublicationLimits::default())
            .unwrap();

        let (replacement_source, replacement_member) = fixture_at_generation("object-a", 2);
        let replacement =
            CatalogSourceProjection::assemble(replacement_source, vec![replacement_member])
                .unwrap();
        let replacement_batch =
            CatalogRefreshProjectionBatch::assemble(vec![replacement], selection(), 3, &prior)
                .unwrap();
        let replacement_publication = replacement_batch
            .reducer
            .freeze_for_initial_publication(CatalogReducerPublicationLimits::default())
            .unwrap();
        assert_eq!(replacement_publication.project_row_count(), 1);
        assert_eq!(replacement_publication.session_row_count(), 1);
        assert_eq!(replacement_publication.tombstone_count(), 0);
        assert!(replacement_publication
            .live_owners()
            .iter()
            .all(|owner| owner.generation == 2));

        let deletion_batch = CatalogRefreshProjectionBatch::assemble(
            vec![deleted_fixture("object-a")],
            selection(),
            3,
            &prior,
        )
        .unwrap();
        let deletion_publication = deletion_batch
            .reducer
            .freeze_for_initial_publication(CatalogReducerPublicationLimits::default())
            .unwrap();
        assert_eq!(deletion_publication.project_row_count(), 0);
        assert_eq!(deletion_publication.session_row_count(), 0);
        assert_eq!(deletion_publication.tombstone_count(), 2);
        assert!(deletion_publication.live_owners().is_empty());
    }
}
