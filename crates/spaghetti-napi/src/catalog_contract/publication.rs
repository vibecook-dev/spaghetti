//! Store-independent RFC 012B initial catalog publication assembly and frames.
//!
//! This module validates the complete semantic payload and projects it into
//! bounded canonical private frames consumed by the B3 writer. It deliberately
//! owns no SQLite, source reads, snapshot transition, or public transport.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize, Serializer};

use super::evidence::{
    serialize_private_json_bounded, CatalogAssertionKey, CatalogEntityKind, CatalogEntityRef,
    CatalogEvidenceOwner, CatalogReducer, CatalogReducerPublication,
    CatalogReducerPublicationLimits,
};
use super::query::CATALOG_BASE_MODEL_MAJOR;
use super::{
    validate_identifier, CatalogContractError, CatalogCoveragePlan, CatalogCoveragePlanId,
    CatalogCoveragePlanSource, CatalogCoverageScope, CatalogReadinessPhase, CatalogReadinessReason,
    CatalogReadinessSnapshot, CatalogSnapshotId, CATALOG_PROJECTION_PACK_ID,
    CATALOG_QUERY_PACK_CONTRACT_VERSION, DIGEST_BYTES,
};
use crate::adapter::{
    ContractVersionSelection, CoverageDomain, CoverageSetCompleteness, SourceCoverageSet,
    CONTRACT_VERSION_SELECTION_VERSION, EXTERNAL_ENTITY_REFERENCE_VERSION,
    SEMANTIC_REFERENCE_CONTRACT_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
};

pub(crate) const CATALOG_INITIAL_PUBLICATION_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_REFRESH_PUBLICATION_CONTRACT_VERSION: u32 = 1;
pub(crate) const CATALOG_DURABLE_REFRESH_PUBLICATION_CONTRACT_VERSION: u32 = 2;

const MAX_PUBLICATION_MEMBERS: usize = 1_000_000;
const MAX_SELECTED_FACT_FAMILIES: usize = 4_096;
pub(crate) const MAX_DURABLE_PUBLICATION_ENTRIES: usize = 2_100_000;
pub(crate) const MAX_DURABLE_PUBLICATION_BYTES: usize = 512 * 1024 * 1024;
/// Internal corruption/retention ceiling for one canonical project or session
/// row. This is a safety bound, not a performance target or public limit.
pub(crate) const MAX_DURABLE_CATALOG_ROW_BYTES: usize = 64 * 1024 * 1024;

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

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&format!("v1:{}", URL_SAFE_NO_PAD.encode(self.0)))
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
private_digest_type!(
    CatalogRefreshPublicationDigest,
    "catalog-refresh-publication-v1"
);
private_digest_type!(CatalogMemberHistoryRevision, "catalog-member-history-v1");

impl fmt::Display for CatalogInitialPublicationDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "catalog-initial-publication-v1:{}",
            URL_SAFE_NO_PAD.encode(self.0)
        )
    }
}

impl CatalogInitialPublicationDigest {
    pub(crate) fn storage_bytes(&self) -> &[u8; DIGEST_BYTES] {
        self.as_bytes()
    }
}

impl CatalogRefreshPublicationDigest {
    pub(crate) fn storage_bytes(&self) -> &[u8; DIGEST_BYTES] {
        self.as_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CatalogDurablePublicationEntryKind {
    Source,
    MemberBinding,
    MemberHistory,
    ReducerState,
    ProjectRow,
    SessionRow,
    Tombstone,
}

impl CatalogDurablePublicationEntryKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::MemberBinding => "member_binding",
            Self::MemberHistory => "member_history",
            Self::ReducerState => "reducer_state",
            Self::ProjectRow => "project_row",
            Self::SessionRow => "session_row",
            Self::Tombstone => "tombstone",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, CatalogContractError> {
        match value {
            "source" => Ok(Self::Source),
            "member_binding" => Ok(Self::MemberBinding),
            "member_history" => Ok(Self::MemberHistory),
            "reducer_state" => Ok(Self::ReducerState),
            "project_row" => Ok(Self::ProjectRow),
            "session_row" => Ok(Self::SessionRow),
            "tombstone" => Ok(Self::Tombstone),
            _ => Err(CatalogContractError::invalid(
                "unsupported durable catalog publication entry kind",
            )),
        }
    }
}

/// One canonical, versioned private frame written by B3 persistence. Payloads
/// can retain local-sensitive values and therefore never implement `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogDurablePublicationEntry {
    kind: CatalogDurablePublicationEntryKind,
    key: [u8; DIGEST_BYTES],
    payload: Vec<u8>,
    payload_digest: [u8; DIGEST_BYTES],
}

impl CatalogDurablePublicationEntry {
    pub(crate) fn kind(&self) -> CatalogDurablePublicationEntryKind {
        self.kind
    }

    pub(crate) fn key(&self) -> &[u8; DIGEST_BYTES] {
        &self.key
    }

    pub(crate) fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub(crate) fn payload_digest(&self) -> &[u8; DIGEST_BYTES] {
        &self.payload_digest
    }
}

/// Checked private durable projection of the non-serializable publication
/// envelope. It is prepared completely before the writer starts SQLite.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogDurableInitialPublication {
    contract_version: u32,
    build: CatalogInitialBuildExpectation,
    contract_selection: ContractVersionSelection,
    contract_selection_json: Vec<u8>,
    member_identity_contract_id: Option<String>,
    source_coverage: Vec<SourceCoverageSet>,
    entries: Vec<CatalogDurablePublicationEntry>,
    publication_digest: CatalogInitialPublicationDigest,
    reducer_revision: super::evidence::CatalogReducerPublicationRevision,
    source_count: usize,
    member_count: usize,
    project_row_count: usize,
    session_row_count: usize,
    tombstone_count: usize,
    encoded_bytes: usize,
    entries_digest: [u8; DIGEST_BYTES],
    content_digest: [u8; DIGEST_BYTES],
}

impl CatalogDurableInitialPublication {
    pub(crate) fn contract_version(&self) -> u32 {
        self.contract_version
    }

    pub(crate) fn build(&self) -> CatalogInitialBuildExpectation {
        self.build
    }

    pub(crate) fn contract_selection(&self) -> &ContractVersionSelection {
        &self.contract_selection
    }

    pub(crate) fn contract_selection_json(&self) -> &[u8] {
        &self.contract_selection_json
    }

    pub(crate) fn member_identity_contract_id(&self) -> Option<&str> {
        self.member_identity_contract_id.as_deref()
    }

    pub(crate) fn source_coverage(&self) -> &[SourceCoverageSet] {
        &self.source_coverage
    }

    pub(crate) fn entries(&self) -> &[CatalogDurablePublicationEntry] {
        &self.entries
    }

    pub(crate) fn publication_digest(&self) -> CatalogInitialPublicationDigest {
        self.publication_digest
    }

    pub(crate) fn reducer_revision(&self) -> super::evidence::CatalogReducerPublicationRevision {
        self.reducer_revision
    }

    pub(crate) fn source_count(&self) -> usize {
        self.source_count
    }

    pub(crate) fn member_count(&self) -> usize {
        self.member_count
    }

    pub(crate) fn project_row_count(&self) -> usize {
        self.project_row_count
    }

    pub(crate) fn session_row_count(&self) -> usize {
        self.session_row_count
    }

    pub(crate) fn tombstone_count(&self) -> usize {
        self.tombstone_count
    }

    pub(crate) fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    pub(crate) fn entries_digest(&self) -> &[u8; DIGEST_BYTES] {
        &self.entries_digest
    }

    pub(crate) fn content_digest(&self) -> &[u8; DIGEST_BYTES] {
        &self.content_digest
    }
}

impl fmt::Debug for CatalogDurableInitialPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogDurableInitialPublication")
            .field("contract_version", &self.contract_version)
            .field("build", &self.build)
            .field("source_count", &self.source_count)
            .field("member_count", &self.member_count)
            .field("project_row_count", &self.project_row_count)
            .field("session_row_count", &self.session_row_count)
            .field("tombstone_count", &self.tombstone_count)
            .field("entry_count", &self.entries.len())
            .field("encoded_bytes", &self.encoded_bytes)
            .field("publication_digest", &self.publication_digest)
            .field("payloads", &"<redacted>")
            .finish()
    }
}

/// Checked durable v2 projection for one ordinary-refresh successor. It
/// retains the exact predecessor commitment and cumulative member history;
/// payload bytes remain private and Debug-redacted.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogDurableRefreshPublication {
    contract_version: u32,
    build: CatalogRefreshBuildExpectation,
    predecessor: CatalogRefreshPredecessor,
    contract_selection: ContractVersionSelection,
    contract_selection_json: Vec<u8>,
    member_identity_contract_id: Option<String>,
    member_history: CatalogPublicationMemberHistory,
    source_coverage: Vec<SourceCoverageSet>,
    entries: Vec<CatalogDurablePublicationEntry>,
    publication_digest: CatalogRefreshPublicationDigest,
    reducer_revision: super::evidence::CatalogReducerPublicationRevision,
    source_count: usize,
    member_count: usize,
    project_row_count: usize,
    session_row_count: usize,
    tombstone_count: usize,
    encoded_bytes: usize,
    entries_digest: [u8; DIGEST_BYTES],
    content_digest: [u8; DIGEST_BYTES],
}

impl CatalogDurableRefreshPublication {
    pub(crate) fn contract_version(&self) -> u32 {
        self.contract_version
    }

    pub(crate) fn build(&self) -> CatalogRefreshBuildExpectation {
        self.build
    }

    pub(crate) fn predecessor(&self) -> &CatalogRefreshPredecessor {
        &self.predecessor
    }

    pub(crate) fn contract_selection(&self) -> &ContractVersionSelection {
        &self.contract_selection
    }

    pub(crate) fn contract_selection_json(&self) -> &[u8] {
        &self.contract_selection_json
    }

    pub(crate) fn member_identity_contract_id(&self) -> Option<&str> {
        self.member_identity_contract_id.as_deref()
    }

    pub(crate) fn member_history(&self) -> &CatalogPublicationMemberHistory {
        &self.member_history
    }

    pub(crate) fn source_coverage(&self) -> &[SourceCoverageSet] {
        &self.source_coverage
    }

    pub(crate) fn entries(&self) -> &[CatalogDurablePublicationEntry] {
        &self.entries
    }

    pub(crate) fn publication_digest(&self) -> CatalogRefreshPublicationDigest {
        self.publication_digest
    }

    pub(crate) fn reducer_revision(&self) -> super::evidence::CatalogReducerPublicationRevision {
        self.reducer_revision
    }

    pub(crate) fn source_count(&self) -> usize {
        self.source_count
    }

    pub(crate) fn member_count(&self) -> usize {
        self.member_count
    }

    pub(crate) fn project_row_count(&self) -> usize {
        self.project_row_count
    }

    pub(crate) fn session_row_count(&self) -> usize {
        self.session_row_count
    }

    pub(crate) fn tombstone_count(&self) -> usize {
        self.tombstone_count
    }

    pub(crate) fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    pub(crate) fn entries_digest(&self) -> &[u8; DIGEST_BYTES] {
        &self.entries_digest
    }

    pub(crate) fn content_digest(&self) -> &[u8; DIGEST_BYTES] {
        &self.content_digest
    }
}

impl fmt::Debug for CatalogDurableRefreshPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogDurableRefreshPublication")
            .field("contract_version", &self.contract_version)
            .field("build", &self.build)
            .field("predecessor", &self.predecessor)
            .field("source_count", &self.source_count)
            .field("member_count", &self.member_count)
            .field("member_history", &self.member_history)
            .field("project_row_count", &self.project_row_count)
            .field("session_row_count", &self.session_row_count)
            .field("tombstone_count", &self.tombstone_count)
            .field("entry_count", &self.entries.len())
            .field("encoded_bytes", &self.encoded_bytes)
            .field("publication_digest", &self.publication_digest)
            .field("payloads", &"<redacted>")
            .finish()
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

/// One cumulative, privacy-safe member-identity commitment. Refreshes may
/// add entries, but an existing opaque member reference can never name a
/// different concrete base session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct CatalogPublicationMemberHistoryEntry {
    member_ref: CatalogPublicationMemberRef,
    session_ref: CatalogEntityRef,
}

/// Canonical cumulative member identity retained independently from the
/// current snapshot's admitting source frames.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogPublicationMemberHistory {
    entries: Vec<CatalogPublicationMemberHistoryEntry>,
    revision: CatalogMemberHistoryRevision,
}

impl CatalogPublicationMemberHistory {
    pub(crate) fn from_bindings(
        bindings: &[CatalogPublicationMemberBinding],
    ) -> Result<Self, CatalogContractError> {
        let mut by_member = BTreeMap::new();
        for binding in bindings {
            if by_member
                .insert(binding.member_ref, binding.session_ref)
                .is_some_and(|existing| existing != binding.session_ref)
            {
                return Err(CatalogContractError::invalid(
                    "one catalog member reference cannot retarget across sources",
                ));
            }
        }
        Self::from_entries(
            by_member
                .into_iter()
                .map(
                    |(member_ref, session_ref)| CatalogPublicationMemberHistoryEntry {
                        member_ref,
                        session_ref,
                    },
                )
                .collect(),
        )
    }

    fn successor(
        &self,
        bindings: &[CatalogPublicationMemberBinding],
    ) -> Result<Self, CatalogContractError> {
        let mut by_member = self
            .entries
            .iter()
            .map(|entry| (entry.member_ref, entry.session_ref))
            .collect::<BTreeMap<_, _>>();
        for binding in bindings {
            match by_member.get(&binding.member_ref) {
                Some(existing) if *existing != binding.session_ref => {
                    return Err(CatalogContractError::invalid(
                        "catalog refresh cannot retarget a historical member reference",
                    ));
                }
                Some(_) => {}
                None => {
                    by_member.insert(binding.member_ref, binding.session_ref);
                }
            }
        }
        Self::from_entries(
            by_member
                .into_iter()
                .map(
                    |(member_ref, session_ref)| CatalogPublicationMemberHistoryEntry {
                        member_ref,
                        session_ref,
                    },
                )
                .collect(),
        )
    }

    pub(crate) fn from_entries(
        entries: Vec<CatalogPublicationMemberHistoryEntry>,
    ) -> Result<Self, CatalogContractError> {
        if entries.len() > MAX_PUBLICATION_MEMBERS
            || !entries
                .windows(2)
                .all(|pair| pair[0].member_ref < pair[1].member_ref)
        {
            return Err(CatalogContractError::invalid(
                "catalog member history is outside its canonical bounded form",
            ));
        }
        for entry in &entries {
            if entry.member_ref.as_bytes().iter().all(|byte| *byte == 0)
                || entry.session_ref.kind != CatalogEntityKind::Session
            {
                return Err(CatalogContractError::invalid(
                    "catalog member history requires nonzero members and concrete sessions",
                ));
            }
            entry.session_ref.validate()?;
        }
        let revision = derive_member_history_revision(&entries);
        Ok(Self { entries, revision })
    }

    pub(crate) fn revision(&self) -> CatalogMemberHistoryRevision {
        self.revision
    }

    pub(crate) fn entries(&self) -> &[CatalogPublicationMemberHistoryEntry] {
        &self.entries
    }
}

impl fmt::Debug for CatalogPublicationMemberHistory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogPublicationMemberHistory")
            .field("entry_count", &self.entries.len())
            .field("revision", &self.revision)
            .finish()
    }
}

/// Exact restart-validated predecessor frozen into an ordinary refresh
/// assembly. The raw digests are opaque and Debug deliberately redacts them.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogRefreshPredecessor {
    snapshot_id: CatalogSnapshotId,
    publication_digest: [u8; DIGEST_BYTES],
    content_digest: [u8; DIGEST_BYTES],
    contract_selection: ContractVersionSelection,
    member_identity_contract_id: Option<String>,
    reducer_revision: super::evidence::CatalogReducerPublicationRevision,
    member_history_revision: CatalogMemberHistoryRevision,
    plan_replacement: bool,
}

impl CatalogRefreshPredecessor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        snapshot_id: CatalogSnapshotId,
        publication_digest: [u8; DIGEST_BYTES],
        content_digest: [u8; DIGEST_BYTES],
        contract_selection: ContractVersionSelection,
        member_identity_contract_id: Option<String>,
        reducer_revision: super::evidence::CatalogReducerPublicationRevision,
        member_history_revision: CatalogMemberHistoryRevision,
    ) -> Result<Self, CatalogContractError> {
        Self::new_with_plan_lineage(
            snapshot_id,
            publication_digest,
            content_digest,
            contract_selection,
            member_identity_contract_id,
            reducer_revision,
            member_history_revision,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_coverage_plan_replacement(
        snapshot_id: CatalogSnapshotId,
        publication_digest: [u8; DIGEST_BYTES],
        content_digest: [u8; DIGEST_BYTES],
        contract_selection: ContractVersionSelection,
        member_identity_contract_id: Option<String>,
        reducer_revision: super::evidence::CatalogReducerPublicationRevision,
        member_history_revision: CatalogMemberHistoryRevision,
    ) -> Result<Self, CatalogContractError> {
        Self::new_with_plan_lineage(
            snapshot_id,
            publication_digest,
            content_digest,
            contract_selection,
            member_identity_contract_id,
            reducer_revision,
            member_history_revision,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_plan_lineage(
        snapshot_id: CatalogSnapshotId,
        publication_digest: [u8; DIGEST_BYTES],
        content_digest: [u8; DIGEST_BYTES],
        contract_selection: ContractVersionSelection,
        member_identity_contract_id: Option<String>,
        reducer_revision: super::evidence::CatalogReducerPublicationRevision,
        member_history_revision: CatalogMemberHistoryRevision,
        plan_replacement: bool,
    ) -> Result<Self, CatalogContractError> {
        CatalogSnapshotId::new(
            snapshot_id.pack_contract_version,
            snapshot_id.coverage_plan_id,
            snapshot_id.readiness_epoch,
            snapshot_id.complete_commit,
        )?;
        validate_contract_selection(&contract_selection)?;
        if publication_digest.iter().all(|byte| *byte == 0)
            || content_digest.iter().all(|byte| *byte == 0)
            || reducer_revision
                .storage_bytes()
                .iter()
                .all(|byte| *byte == 0)
            || member_history_revision
                .as_bytes()
                .iter()
                .all(|byte| *byte == 0)
            || contract_selection.query_pack_version != Some(snapshot_id.pack_contract_version)
        {
            return Err(CatalogContractError::invalid(
                "catalog refresh predecessor has invalid digest or selection lineage",
            ));
        }
        if let Some(identity_contract) = &member_identity_contract_id {
            validate_identifier("catalog member identity contract id", identity_contract)?;
        }
        Ok(Self {
            snapshot_id,
            publication_digest,
            content_digest,
            contract_selection,
            member_identity_contract_id,
            reducer_revision,
            member_history_revision,
            plan_replacement,
        })
    }

    pub(crate) fn snapshot_id(&self) -> CatalogSnapshotId {
        self.snapshot_id
    }

    pub(crate) fn publication_digest(&self) -> &[u8; DIGEST_BYTES] {
        &self.publication_digest
    }

    pub(crate) fn content_digest(&self) -> &[u8; DIGEST_BYTES] {
        &self.content_digest
    }

    pub(crate) fn reducer_revision(&self) -> super::evidence::CatalogReducerPublicationRevision {
        self.reducer_revision
    }

    pub(crate) fn member_history_revision(&self) -> CatalogMemberHistoryRevision {
        self.member_history_revision
    }

    pub(crate) fn is_plan_replacement(&self) -> bool {
        self.plan_replacement
    }
}

impl fmt::Debug for CatalogRefreshPredecessor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogRefreshPredecessor")
            .field("snapshot_id", &self.snapshot_id)
            .field("contract_selection", &self.contract_selection)
            .field(
                "member_identity_contract_id",
                &self.member_identity_contract_id,
            )
            .field("reducer_revision", &self.reducer_revision)
            .field("member_history_revision", &self.member_history_revision)
            .field("plan_replacement", &self.plan_replacement)
            .field("digests", &"<opaque>")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableSourceWire {
    durable_source_contract_version: u32,
    plan_source: CatalogCoveragePlanSource,
    contract_selection: ContractVersionSelection,
    member_identity_contract_id: String,
    membership_revision: String,
    component_completion_revision: String,
    member_count: usize,
    source_coverage: SourceCoverageSet,
    source_digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableMemberBindingWire {
    durable_member_binding_contract_version: u32,
    source: CatalogCoveragePlanSource,
    member_ref: String,
    assertion_key: CatalogAssertionKey,
    session_ref: CatalogEntityRef,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableMemberHistoryEntryWire {
    member_ref: String,
    session_ref: CatalogEntityRef,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableMemberHistoryWire {
    durable_member_history_contract_version: u32,
    entries: Vec<DurableMemberHistoryEntryWire>,
    history_revision: String,
}

/// Checked source frame awaiting the member references carried by the
/// separately keyed membership frames. Debug is intentionally omitted because
/// coverage can retain local object coordinates.
pub(crate) struct CatalogDurableSourceFrame {
    plan_source: CatalogCoveragePlanSource,
    contract_selection: ContractVersionSelection,
    member_identity_contract_id: String,
    membership_revision: CatalogSourceMembershipRevision,
    component_completion_revision: CatalogSourceCompletionRevision,
    member_count: usize,
    source_coverage: SourceCoverageSet,
    digest: CatalogCompleteSourceDigest,
}

impl CatalogDurableSourceFrame {
    fn complete(
        self,
        member_refs: Vec<CatalogPublicationMemberRef>,
    ) -> Result<CatalogCompleteSourceAssembly, CatalogContractError> {
        if member_refs.len() != self.member_count {
            return Err(CatalogContractError::invalid(
                "durable catalog source member count does not match its member frames",
            ));
        }
        let source = CatalogCompleteSourceAssembly::from_complete_library_coverage(
            self.plan_source,
            self.contract_selection,
            self.member_identity_contract_id,
            self.membership_revision,
            self.component_completion_revision,
            member_refs,
            self.source_coverage,
        )?;
        if source.digest != self.digest {
            return Err(CatalogContractError::invalid(
                "durable catalog source digest does not match its completed membership evidence",
            ));
        }
        Ok(source)
    }
}

pub(crate) fn decode_durable_source_frame(
    payload: &[u8],
    expected_key: &[u8; DIGEST_BYTES],
    max_payload_bytes: usize,
) -> Result<CatalogDurableSourceFrame, CatalogContractError> {
    if payload.is_empty() || payload.len() > max_payload_bytes {
        return Err(CatalogContractError::invalid(
            "durable catalog source payload is outside its byte bound",
        ));
    }
    let wire: DurableSourceWire = serde_json::from_slice(payload).map_err(|error| {
        CatalogContractError::invalid(format!("durable catalog source is invalid: {error}"))
    })?;
    let canonical = serialize_private_json_bounded(&wire, payload.len(), "durable catalog source")?;
    let membership_revision = decode_private_digest(
        &wire.membership_revision,
        "durable catalog source membership revision",
    )?;
    let component_completion_revision = decode_private_digest(
        &wire.component_completion_revision,
        "durable catalog source component completion revision",
    )?;
    let source_digest =
        decode_private_digest(&wire.source_digest, "durable catalog source digest")?;
    if canonical != payload
        || wire.durable_source_contract_version != CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION
        || &source_digest != expected_key
        || wire.member_count > MAX_PUBLICATION_MEMBERS
    {
        return Err(CatalogContractError::invalid(
            "durable catalog source frame is noncanonical or outside its frozen bounds",
        ));
    }
    wire.plan_source.validate()?;
    validate_contract_selection(&wire.contract_selection)?;
    validate_identifier(
        "catalog member identity contract id",
        &wire.member_identity_contract_id,
    )?;
    validate_complete_source_coverage(
        &wire.plan_source,
        &wire.contract_selection,
        &wire.source_coverage,
    )?;
    Ok(CatalogDurableSourceFrame {
        plan_source: wire.plan_source,
        contract_selection: wire.contract_selection,
        member_identity_contract_id: wire.member_identity_contract_id,
        membership_revision: CatalogSourceMembershipRevision(membership_revision),
        component_completion_revision: CatalogSourceCompletionRevision(
            component_completion_revision,
        ),
        member_count: wire.member_count,
        source_coverage: wire.source_coverage,
        digest: CatalogCompleteSourceDigest(source_digest),
    })
}

fn decode_private_digest(
    value: &str,
    field: &'static str,
) -> Result<[u8; DIGEST_BYTES], CatalogContractError> {
    if value.len() != 46 {
        return Err(CatalogContractError::invalid(format!(
            "{field} must use the exact v1 digest width"
        )));
    }
    let encoded = value.strip_prefix("v1:").ok_or_else(|| {
        CatalogContractError::invalid(format!("{field} has an unsupported encoding"))
    })?;
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| CatalogContractError::invalid(format!("{field} is not valid base64url")))?;
    let digest: [u8; DIGEST_BYTES] = decoded.try_into().map_err(|_| {
        CatalogContractError::invalid(format!("{field} must contain exactly 32 bytes"))
    })?;
    if digest.iter().all(|byte| *byte == 0) {
        return Err(CatalogContractError::invalid(format!(
            "{field} must be nonzero"
        )));
    }
    Ok(digest)
}

pub(crate) fn decode_durable_member_binding_frame(
    payload: &[u8],
    expected_key: &[u8; DIGEST_BYTES],
    max_payload_bytes: usize,
) -> Result<CatalogPublicationMemberBinding, CatalogContractError> {
    if payload.is_empty() || payload.len() > max_payload_bytes {
        return Err(CatalogContractError::invalid(
            "durable catalog member-binding payload is outside its byte bound",
        ));
    }
    let wire: DurableMemberBindingWire = serde_json::from_slice(payload).map_err(|error| {
        CatalogContractError::invalid(format!(
            "durable catalog member binding is invalid: {error}"
        ))
    })?;
    let canonical =
        serialize_private_json_bounded(&wire, payload.len(), "durable catalog member binding")?;
    let member_ref = decode_private_digest(
        &wire.member_ref,
        "durable catalog publication member reference",
    )?;
    if canonical != payload
        || wire.durable_member_binding_contract_version
            != CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION
        || wire
            .assertion_key
            .publication_bytes()
            .iter()
            .all(|byte| *byte == 0)
    {
        return Err(CatalogContractError::invalid(
            "durable catalog member binding is noncanonical or outside its frozen contract",
        ));
    }
    let binding = CatalogPublicationMemberBinding::new(
        wire.source,
        CatalogPublicationMemberRef(member_ref),
        wire.assertion_key,
        wire.session_ref,
    )?;
    if &derive_member_binding_frame_key(&binding) != expected_key {
        return Err(CatalogContractError::invalid(
            "durable catalog member binding does not match its frame key",
        ));
    }
    Ok(binding)
}

pub(crate) fn decode_durable_member_history_frame(
    payload: &[u8],
    expected_key: &[u8; DIGEST_BYTES],
    max_payload_bytes: usize,
) -> Result<CatalogPublicationMemberHistory, CatalogContractError> {
    if payload.is_empty() || payload.len() > max_payload_bytes {
        return Err(CatalogContractError::invalid(
            "durable catalog member-history payload is outside its byte bound",
        ));
    }
    let wire: DurableMemberHistoryWire = serde_json::from_slice(payload).map_err(|error| {
        CatalogContractError::invalid(format!(
            "durable catalog member history is invalid: {error}"
        ))
    })?;
    if wire.entries.len() > MAX_PUBLICATION_MEMBERS {
        return Err(CatalogContractError::invalid(
            "durable catalog member history exceeds its entry bound",
        ));
    }
    let canonical =
        serialize_private_json_bounded(&wire, payload.len(), "durable catalog member history")?;
    let declared_revision = decode_private_digest(
        &wire.history_revision,
        "durable catalog member history revision",
    )?;
    if canonical != payload
        || wire.durable_member_history_contract_version != 1
        || &declared_revision != expected_key
    {
        return Err(CatalogContractError::invalid(
            "durable catalog member history is noncanonical or outside its frozen contract",
        ));
    }
    let entries = wire
        .entries
        .into_iter()
        .map(|entry| {
            Ok(CatalogPublicationMemberHistoryEntry {
                member_ref: CatalogPublicationMemberRef(decode_private_digest(
                    &entry.member_ref,
                    "durable catalog member reference history",
                )?),
                session_ref: entry.session_ref,
            })
        })
        .collect::<Result<Vec<_>, CatalogContractError>>()?;
    let history = CatalogPublicationMemberHistory::from_entries(entries)?;
    if history.revision.as_bytes() != expected_key {
        return Err(CatalogContractError::invalid(
            "durable catalog member history revision does not match its entries",
        ));
    }
    Ok(history)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogInitialBuildExpectation {
    pub coverage_plan_id: CatalogCoveragePlanId,
    pub desired_contract_version: u32,
    pub epoch: u64,
    pub attempt: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CatalogRefreshBuildExpectation {
    pub coverage_plan_id: CatalogCoveragePlanId,
    pub desired_contract_version: u32,
    pub epoch: u64,
    pub attempt: u64,
    pub refresh_started_commit_seq: u64,
    pub predecessor_snapshot: CatalogSnapshotId,
    pub plan_replacement: bool,
}

/// Fully checked, store-independent input for one atomic initial catalog
/// publication. The raw reducer evidence and rows remain private and Debug is
/// redacted; the writer consumes this checked value rather than
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

    pub(crate) fn reducer_publication(&self) -> &CatalogReducerPublication {
        &self.reducer
    }

    pub(crate) fn member_history(
        &self,
    ) -> Result<CatalogPublicationMemberHistory, CatalogContractError> {
        CatalogPublicationMemberHistory::from_bindings(&self.member_bindings)
    }

    /// Project the checked store-free envelope into bounded, canonical private
    /// durable frames. Encoding completes before the writer opens a
    /// transaction, so an oversized publication cannot create a partial
    /// SQLite lineage.
    pub(crate) fn prepare_durable(
        &self,
    ) -> Result<CatalogDurableInitialPublication, CatalogContractError> {
        self.prepare_durable_with_limits(
            MAX_DURABLE_PUBLICATION_ENTRIES,
            MAX_DURABLE_PUBLICATION_BYTES,
        )
    }

    #[cfg(test)]
    pub(crate) fn prepare_durable_with_test_limits(
        &self,
        max_entries: usize,
        max_encoded_bytes: usize,
    ) -> Result<CatalogDurableInitialPublication, CatalogContractError> {
        self.prepare_durable_with_limits(max_entries, max_encoded_bytes)
    }

    fn prepare_durable_with_limits(
        &self,
        max_entries: usize,
        max_encoded_bytes: usize,
    ) -> Result<CatalogDurableInitialPublication, CatalogContractError> {
        if max_entries == 0
            || max_entries > MAX_DURABLE_PUBLICATION_ENTRIES
            || max_encoded_bytes == 0
            || max_encoded_bytes > MAX_DURABLE_PUBLICATION_BYTES
        {
            return Err(CatalogContractError::invalid(
                "catalog durable publication limits are outside the frozen safety ceilings",
            ));
        }
        #[derive(Serialize)]
        struct DurableSource<'a> {
            durable_source_contract_version: u32,
            plan_source: &'a CatalogCoveragePlanSource,
            contract_selection: &'a ContractVersionSelection,
            member_identity_contract_id: &'a str,
            membership_revision: CatalogSourceMembershipRevision,
            component_completion_revision: CatalogSourceCompletionRevision,
            member_count: usize,
            source_coverage: &'a SourceCoverageSet,
            source_digest: CatalogCompleteSourceDigest,
        }

        #[derive(Serialize)]
        struct DurableMemberBinding<'a> {
            durable_member_binding_contract_version: u32,
            source: &'a CatalogCoveragePlanSource,
            member_ref: CatalogPublicationMemberRef,
            assertion_key: CatalogAssertionKey,
            session_ref: CatalogEntityRef,
        }

        let identity_contract_bytes = self
            .member_identity_contract_id
            .as_ref()
            .map_or(0, String::len);
        let selection_budget = max_encoded_bytes
            .checked_sub(identity_contract_bytes)
            .ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog member identity contract exhausts the durable byte ceiling",
                )
            })?;
        let contract_selection_json = serialize_private_json_bounded(
            &self.contract_selection,
            selection_budget,
            "catalog publication contract selection",
        )?;
        let mut encoded_bytes = contract_selection_json
            .len()
            .checked_add(identity_contract_bytes)
            .ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog durable publication encoded byte count overflow",
                )
            })?;
        let mut entries = Vec::new();
        for source in &self.sources {
            serialize_and_push_durable_entry(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                CatalogDurablePublicationEntryKind::Source,
                *source.digest.as_bytes(),
                &DurableSource {
                    durable_source_contract_version: CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION,
                    plan_source: &source.plan_source,
                    contract_selection: &source.contract_selection,
                    member_identity_contract_id: &source.member_identity_contract_id,
                    membership_revision: source.membership_revision,
                    component_completion_revision: source.component_completion_revision,
                    member_count: source.member_refs.len(),
                    source_coverage: &source.source_coverage,
                    source_digest: source.digest,
                },
                "catalog complete source",
            )?;
        }
        for binding in &self.member_bindings {
            serialize_and_push_durable_entry(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                CatalogDurablePublicationEntryKind::MemberBinding,
                derive_member_binding_frame_key(binding),
                &DurableMemberBinding {
                    durable_member_binding_contract_version:
                        CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION,
                    source: &binding.source,
                    member_ref: binding.member_ref,
                    assertion_key: binding.assertion_key,
                    session_ref: binding.session_ref,
                },
                "catalog member binding",
            )?;
        }

        let reducer_budget = durable_entry_payload_budget(
            entries.len(),
            encoded_bytes,
            max_entries,
            max_encoded_bytes,
            CatalogDurablePublicationEntryKind::ReducerState,
        )?;
        let reducer_state = self.reducer.durable_state_json(reducer_budget)?;
        push_durable_entry(
            &mut entries,
            &mut encoded_bytes,
            max_entries,
            max_encoded_bytes,
            CatalogDurablePublicationEntryKind::ReducerState,
            *self.reducer.revision().storage_bytes(),
            reducer_state,
        )?;
        for row in self.reducer.project_rows() {
            row.validate_for_durable()?;
            serialize_and_push_durable_entry_with_payload_limit(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                MAX_DURABLE_CATALOG_ROW_BYTES,
                CatalogDurablePublicationEntryKind::ProjectRow,
                *row.project_ref.external_ref.entity_key.as_bytes(),
                row,
                "catalog project row",
            )?;
        }
        for row in self.reducer.session_rows() {
            row.validate_for_durable()?;
            serialize_and_push_durable_entry_with_payload_limit(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                MAX_DURABLE_CATALOG_ROW_BYTES,
                CatalogDurablePublicationEntryKind::SessionRow,
                *row.session_ref.external_ref.entity_key.as_bytes(),
                row,
                "catalog session row",
            )?;
        }
        for tombstone in self.reducer.tombstones() {
            serialize_and_push_durable_entry(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                CatalogDurablePublicationEntryKind::Tombstone,
                *tombstone.entity_ref.external_ref.entity_key.as_bytes(),
                tombstone,
                "catalog tombstone",
            )?;
        }
        entries.sort_by_key(|entry| (entry.kind, entry.key));
        if entries
            .windows(2)
            .any(|pair| pair[0].kind == pair[1].kind && pair[0].key == pair[1].key)
        {
            return Err(CatalogContractError::invalid(
                "catalog durable publication contains duplicate typed entry keys",
            ));
        }

        let entry_summaries = entries
            .iter()
            .map(|entry| {
                (
                    entry.kind,
                    entry.key,
                    entry.payload.len(),
                    entry.payload_digest,
                )
            })
            .collect::<Vec<_>>();
        let entries_digest = derive_durable_entries_digest(&entry_summaries);
        let content_digest = derive_durable_content_digest(
            self.build,
            &contract_selection_json,
            self.member_identity_contract_id.as_deref(),
            self.digest,
            self.reducer.revision(),
            self.sources.len(),
            self.member_bindings.len(),
            self.reducer.project_row_count(),
            self.reducer.session_row_count(),
            self.reducer.tombstone_count(),
            encoded_bytes,
            entries_digest,
        );
        Ok(CatalogDurableInitialPublication {
            contract_version: CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION,
            build: self.build,
            contract_selection: self.contract_selection.clone(),
            contract_selection_json,
            member_identity_contract_id: self.member_identity_contract_id.clone(),
            source_coverage: self
                .sources
                .iter()
                .map(|source| source.source_coverage.clone())
                .collect(),
            entries,
            publication_digest: self.digest,
            reducer_revision: self.reducer.revision(),
            source_count: self.sources.len(),
            member_count: self.member_bindings.len(),
            project_row_count: self.reducer.project_row_count(),
            session_row_count: self.reducer.session_row_count(),
            tombstone_count: self.reducer.tombstone_count(),
            encoded_bytes,
            entries_digest,
            content_digest,
        })
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

/// Fully checked, store-independent successor for one active ordinary
/// refresh. The predecessor reducer and cumulative member history are part of
/// construction, so a caller cannot silently restart from a fresh reducer or
/// retarget an identity that disappeared from the current source membership.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CatalogRefreshPublicationAssembly {
    contract_version: u32,
    build: CatalogRefreshBuildExpectation,
    predecessor: CatalogRefreshPredecessor,
    contract_selection: ContractVersionSelection,
    member_identity_contract_id: Option<String>,
    sources: Vec<CatalogCompleteSourceAssembly>,
    member_bindings: Vec<CatalogPublicationMemberBinding>,
    member_history: CatalogPublicationMemberHistory,
    reducer: CatalogReducerPublication,
    limits: CatalogPublicationLimits,
    digest: CatalogRefreshPublicationDigest,
}

impl CatalogRefreshPublicationAssembly {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn assemble(
        plan: &CatalogCoveragePlan,
        readiness: &CatalogReadinessSnapshot,
        refresh_started_commit_seq: u64,
        predecessor: CatalogRefreshPredecessor,
        prior_reducer: &CatalogReducerPublication,
        prior_member_history: &CatalogPublicationMemberHistory,
        contract_selection: ContractVersionSelection,
        mut sources: Vec<CatalogCompleteSourceAssembly>,
        reducer: &CatalogReducer,
        mut member_bindings: Vec<CatalogPublicationMemberBinding>,
        limits: CatalogPublicationLimits,
    ) -> Result<Self, CatalogContractError> {
        let build = validate_refresh_build(
            plan,
            readiness,
            refresh_started_commit_seq,
            &predecessor,
            &contract_selection,
        )?;
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
                "catalog refresh contains more complete sources than its frozen plan",
            ));
        }
        sources.sort_by(|left, right| left.plan_source.cmp(&right.plan_source));
        if sources
            .windows(2)
            .any(|pair| pair[0].plan_source == pair[1].plan_source)
        {
            return Err(CatalogContractError::invalid(
                "catalog refresh contains a duplicate complete source assembly",
            ));
        }
        validate_plan_sources(plan, &contract_selection, &sources)?;
        let total_members = sources.iter().try_fold(0_usize, |count, source| {
            count.checked_add(source.member_refs.len()).ok_or_else(|| {
                CatalogContractError::invalid("catalog refresh member count overflow")
            })
        })?;
        if total_members > limits.max_members || member_bindings.len() != total_members {
            return Err(CatalogContractError::invalid(
                "catalog refresh requires one bounded session binding for every admitted member",
            ));
        }
        let member_identity_contract_id = sources
            .first()
            .map(|source| source.member_identity_contract_id.clone());
        if sources.iter().any(|source| {
            Some(source.member_identity_contract_id.as_str())
                != member_identity_contract_id.as_deref()
        }) || member_identity_contract_id != predecessor.member_identity_contract_id
        {
            return Err(CatalogContractError::invalid(
                "catalog refresh cannot drift its member identity contract",
            ));
        }

        let reducer = reducer.freeze_for_initial_publication(limits.reducer)?;
        prior_reducer.validate_refresh_successor(&reducer)?;
        if prior_reducer.revision() != predecessor.reducer_revision {
            return Err(CatalogContractError::invalid(
                "catalog refresh predecessor reducer does not match its retained revision",
            ));
        }
        if prior_member_history.revision() != predecessor.member_history_revision {
            return Err(CatalogContractError::invalid(
                "catalog refresh predecessor member history does not match its retained revision",
            ));
        }
        validate_covered_reducer(&sources, &reducer)?;
        member_bindings.sort_by(|left, right| left.coordinate().cmp(&right.coordinate()));
        validate_member_bindings(&sources, &reducer, &member_bindings)?;
        let member_history = prior_member_history.successor(&member_bindings)?;
        let digest = derive_refresh_publication_digest(
            build,
            &predecessor,
            &contract_selection,
            member_identity_contract_id.as_deref(),
            &sources,
            &member_bindings,
            &member_history,
            &reducer,
            limits,
        );
        Ok(Self {
            contract_version: CATALOG_REFRESH_PUBLICATION_CONTRACT_VERSION,
            build,
            predecessor,
            contract_selection,
            member_identity_contract_id,
            sources,
            member_bindings,
            member_history,
            reducer,
            limits,
            digest,
        })
    }

    pub(crate) fn build(&self) -> CatalogRefreshBuildExpectation {
        self.build
    }

    pub(crate) fn predecessor(&self) -> &CatalogRefreshPredecessor {
        &self.predecessor
    }

    pub(crate) fn digest(&self) -> CatalogRefreshPublicationDigest {
        self.digest
    }

    pub(crate) fn prepare_durable(
        &self,
    ) -> Result<CatalogDurableRefreshPublication, CatalogContractError> {
        self.prepare_durable_with_limits(
            MAX_DURABLE_PUBLICATION_ENTRIES,
            MAX_DURABLE_PUBLICATION_BYTES,
        )
    }

    #[cfg(test)]
    pub(crate) fn prepare_durable_with_test_limits(
        &self,
        max_entries: usize,
        max_encoded_bytes: usize,
    ) -> Result<CatalogDurableRefreshPublication, CatalogContractError> {
        self.prepare_durable_with_limits(max_entries, max_encoded_bytes)
    }

    fn prepare_durable_with_limits(
        &self,
        max_entries: usize,
        max_encoded_bytes: usize,
    ) -> Result<CatalogDurableRefreshPublication, CatalogContractError> {
        if max_entries == 0
            || max_entries > MAX_DURABLE_PUBLICATION_ENTRIES
            || max_encoded_bytes == 0
            || max_encoded_bytes > MAX_DURABLE_PUBLICATION_BYTES
        {
            return Err(CatalogContractError::invalid(
                "catalog durable refresh limits are outside the frozen safety ceilings",
            ));
        }
        #[derive(Serialize)]
        struct DurableSource<'a> {
            durable_source_contract_version: u32,
            plan_source: &'a CatalogCoveragePlanSource,
            contract_selection: &'a ContractVersionSelection,
            member_identity_contract_id: &'a str,
            membership_revision: CatalogSourceMembershipRevision,
            component_completion_revision: CatalogSourceCompletionRevision,
            member_count: usize,
            source_coverage: &'a SourceCoverageSet,
            source_digest: CatalogCompleteSourceDigest,
        }
        #[derive(Serialize)]
        struct DurableMemberBinding<'a> {
            durable_member_binding_contract_version: u32,
            source: &'a CatalogCoveragePlanSource,
            member_ref: CatalogPublicationMemberRef,
            assertion_key: CatalogAssertionKey,
            session_ref: CatalogEntityRef,
        }
        #[derive(Serialize)]
        struct DurableMemberHistory<'a> {
            durable_member_history_contract_version: u32,
            entries: &'a [CatalogPublicationMemberHistoryEntry],
            history_revision: CatalogMemberHistoryRevision,
        }

        let identity_contract_bytes = self
            .member_identity_contract_id
            .as_ref()
            .map_or(0, String::len);
        let selection_budget = max_encoded_bytes
            .checked_sub(identity_contract_bytes)
            .ok_or_else(|| {
                CatalogContractError::invalid(
                    "catalog member identity contract exhausts the durable refresh byte ceiling",
                )
            })?;
        let contract_selection_json = serialize_private_json_bounded(
            &self.contract_selection,
            selection_budget,
            "catalog refresh contract selection",
        )?;
        let mut encoded_bytes = contract_selection_json
            .len()
            .checked_add(identity_contract_bytes)
            .ok_or_else(|| {
                CatalogContractError::invalid("catalog durable refresh byte count overflow")
            })?;
        let mut entries = Vec::new();
        for source in &self.sources {
            serialize_and_push_durable_entry(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                CatalogDurablePublicationEntryKind::Source,
                *source.digest.as_bytes(),
                &DurableSource {
                    durable_source_contract_version: CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION,
                    plan_source: &source.plan_source,
                    contract_selection: &source.contract_selection,
                    member_identity_contract_id: &source.member_identity_contract_id,
                    membership_revision: source.membership_revision,
                    component_completion_revision: source.component_completion_revision,
                    member_count: source.member_refs.len(),
                    source_coverage: &source.source_coverage,
                    source_digest: source.digest,
                },
                "catalog refresh complete source",
            )?;
        }
        for binding in &self.member_bindings {
            serialize_and_push_durable_entry(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                CatalogDurablePublicationEntryKind::MemberBinding,
                derive_member_binding_frame_key(binding),
                &DurableMemberBinding {
                    durable_member_binding_contract_version:
                        CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION,
                    source: &binding.source,
                    member_ref: binding.member_ref,
                    assertion_key: binding.assertion_key,
                    session_ref: binding.session_ref,
                },
                "catalog refresh member binding",
            )?;
        }
        serialize_and_push_durable_entry(
            &mut entries,
            &mut encoded_bytes,
            max_entries,
            max_encoded_bytes,
            CatalogDurablePublicationEntryKind::MemberHistory,
            *self.member_history.revision.as_bytes(),
            &DurableMemberHistory {
                durable_member_history_contract_version: 1,
                entries: &self.member_history.entries,
                history_revision: self.member_history.revision,
            },
            "catalog cumulative member history",
        )?;
        let reducer_budget = durable_entry_payload_budget(
            entries.len(),
            encoded_bytes,
            max_entries,
            max_encoded_bytes,
            CatalogDurablePublicationEntryKind::ReducerState,
        )?;
        let reducer_state = self.reducer.durable_state_json(reducer_budget)?;
        push_durable_entry(
            &mut entries,
            &mut encoded_bytes,
            max_entries,
            max_encoded_bytes,
            CatalogDurablePublicationEntryKind::ReducerState,
            *self.reducer.revision().storage_bytes(),
            reducer_state,
        )?;
        for row in self.reducer.project_rows() {
            row.validate_for_durable()?;
            serialize_and_push_durable_entry_with_payload_limit(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                MAX_DURABLE_CATALOG_ROW_BYTES,
                CatalogDurablePublicationEntryKind::ProjectRow,
                *row.project_ref.external_ref.entity_key.as_bytes(),
                row,
                "catalog refresh project row",
            )?;
        }
        for row in self.reducer.session_rows() {
            row.validate_for_durable()?;
            serialize_and_push_durable_entry_with_payload_limit(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                MAX_DURABLE_CATALOG_ROW_BYTES,
                CatalogDurablePublicationEntryKind::SessionRow,
                *row.session_ref.external_ref.entity_key.as_bytes(),
                row,
                "catalog refresh session row",
            )?;
        }
        for tombstone in self.reducer.tombstones() {
            serialize_and_push_durable_entry(
                &mut entries,
                &mut encoded_bytes,
                max_entries,
                max_encoded_bytes,
                CatalogDurablePublicationEntryKind::Tombstone,
                *tombstone.entity_ref.external_ref.entity_key.as_bytes(),
                tombstone,
                "catalog refresh tombstone",
            )?;
        }
        entries.sort_by_key(|entry| (entry.kind, entry.key));
        if entries
            .windows(2)
            .any(|pair| pair[0].kind == pair[1].kind && pair[0].key == pair[1].key)
        {
            return Err(CatalogContractError::invalid(
                "catalog durable refresh contains duplicate typed entry keys",
            ));
        }
        let entry_summaries = entries
            .iter()
            .map(|entry| {
                (
                    entry.kind,
                    entry.key,
                    entry.payload.len(),
                    entry.payload_digest,
                )
            })
            .collect::<Vec<_>>();
        let entries_digest = derive_durable_entries_digest(&entry_summaries);
        let content_digest = derive_durable_refresh_content_digest(
            self.build,
            &self.predecessor,
            &contract_selection_json,
            self.member_identity_contract_id.as_deref(),
            self.digest,
            self.reducer.revision(),
            self.member_history.revision,
            self.sources.len(),
            self.member_bindings.len(),
            self.reducer.project_row_count(),
            self.reducer.session_row_count(),
            self.reducer.tombstone_count(),
            encoded_bytes,
            entries_digest,
        );
        Ok(CatalogDurableRefreshPublication {
            contract_version: CATALOG_DURABLE_REFRESH_PUBLICATION_CONTRACT_VERSION,
            build: self.build,
            predecessor: self.predecessor.clone(),
            contract_selection: self.contract_selection.clone(),
            contract_selection_json,
            member_identity_contract_id: self.member_identity_contract_id.clone(),
            member_history: self.member_history.clone(),
            source_coverage: self
                .sources
                .iter()
                .map(|source| source.source_coverage.clone())
                .collect(),
            entries,
            publication_digest: self.digest,
            reducer_revision: self.reducer.revision(),
            source_count: self.sources.len(),
            member_count: self.member_bindings.len(),
            project_row_count: self.reducer.project_row_count(),
            session_row_count: self.reducer.session_row_count(),
            tombstone_count: self.reducer.tombstone_count(),
            encoded_bytes,
            entries_digest,
            content_digest,
        })
    }
}

impl fmt::Debug for CatalogRefreshPublicationAssembly {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogRefreshPublicationAssembly")
            .field("contract_version", &self.contract_version)
            .field("build", &self.build)
            .field("predecessor", &self.predecessor)
            .field("source_count", &self.sources.len())
            .field("member_count", &self.member_bindings.len())
            .field("member_history", &self.member_history)
            .field("reducer_revision", &self.reducer.revision())
            .field("digest", &self.digest)
            .finish()
    }
}

fn durable_entry_payload_budget(
    entry_count: usize,
    encoded_bytes: usize,
    max_entries: usize,
    max_encoded_bytes: usize,
    kind: CatalogDurablePublicationEntryKind,
) -> Result<usize, CatalogContractError> {
    if entry_count >= max_entries {
        return Err(CatalogContractError::invalid(format!(
            "catalog durable publication reached its {max_entries}-entry ceiling before {}",
            kind.as_str()
        )));
    }
    let frame_overhead = DIGEST_BYTES
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(kind.as_str().len()))
        .ok_or_else(|| {
            CatalogContractError::invalid(
                "catalog durable publication frame-overhead count overflow",
            )
        })?;
    let after_overhead = encoded_bytes.checked_add(frame_overhead).ok_or_else(|| {
        CatalogContractError::invalid("catalog durable publication encoded byte count overflow")
    })?;
    let remaining = max_encoded_bytes
        .checked_sub(after_overhead)
        .ok_or_else(|| {
            CatalogContractError::invalid(format!(
            "catalog durable publication exhausted its {max_encoded_bytes}-byte ceiling before {}",
            kind.as_str()
        ))
        })?;
    if remaining == 0 {
        return Err(CatalogContractError::invalid(format!(
            "catalog durable publication has no payload budget for {}",
            kind.as_str()
        )));
    }
    Ok(remaining)
}

#[allow(clippy::too_many_arguments)]
fn serialize_and_push_durable_entry<T: Serialize + ?Sized>(
    entries: &mut Vec<CatalogDurablePublicationEntry>,
    encoded_bytes: &mut usize,
    max_entries: usize,
    max_encoded_bytes: usize,
    kind: CatalogDurablePublicationEntryKind,
    key: [u8; DIGEST_BYTES],
    value: &T,
    label: &'static str,
) -> Result<(), CatalogContractError> {
    serialize_and_push_durable_entry_with_payload_limit(
        entries,
        encoded_bytes,
        max_entries,
        max_encoded_bytes,
        MAX_DURABLE_PUBLICATION_BYTES,
        kind,
        key,
        value,
        label,
    )
}

#[allow(clippy::too_many_arguments)]
fn serialize_and_push_durable_entry_with_payload_limit<T: Serialize + ?Sized>(
    entries: &mut Vec<CatalogDurablePublicationEntry>,
    encoded_bytes: &mut usize,
    max_entries: usize,
    max_encoded_bytes: usize,
    max_payload_bytes: usize,
    kind: CatalogDurablePublicationEntryKind,
    key: [u8; DIGEST_BYTES],
    value: &T,
    label: &'static str,
) -> Result<(), CatalogContractError> {
    let payload_budget = durable_entry_payload_budget(
        entries.len(),
        *encoded_bytes,
        max_entries,
        max_encoded_bytes,
        kind,
    )?
    .min(max_payload_bytes);
    let payload = serialize_private_json_bounded(value, payload_budget, label)?;
    push_durable_entry(
        entries,
        encoded_bytes,
        max_entries,
        max_encoded_bytes,
        kind,
        key,
        payload,
    )
}

#[allow(clippy::too_many_arguments)]
fn push_durable_entry(
    entries: &mut Vec<CatalogDurablePublicationEntry>,
    encoded_bytes: &mut usize,
    max_entries: usize,
    max_encoded_bytes: usize,
    kind: CatalogDurablePublicationEntryKind,
    key: [u8; DIGEST_BYTES],
    payload: Vec<u8>,
) -> Result<(), CatalogContractError> {
    let payload_budget = durable_entry_payload_budget(
        entries.len(),
        *encoded_bytes,
        max_entries,
        max_encoded_bytes,
        kind,
    )?;
    if payload.len() > payload_budget {
        return Err(CatalogContractError::invalid(format!(
            "catalog durable {} frame exceeds its remaining payload budget",
            kind.as_str()
        )));
    }
    let entry = durable_entry(kind, key, payload)?;
    *encoded_bytes = encoded_bytes
        .checked_add(entry.payload.len())
        .and_then(|bytes| bytes.checked_add(DIGEST_BYTES * 2))
        .and_then(|bytes| bytes.checked_add(kind.as_str().len()))
        .ok_or_else(|| {
            CatalogContractError::invalid("catalog durable publication encoded byte count overflow")
        })?;
    debug_assert!(*encoded_bytes <= max_encoded_bytes);
    entries.push(entry);
    Ok(())
}

fn durable_entry(
    kind: CatalogDurablePublicationEntryKind,
    key: [u8; DIGEST_BYTES],
    payload: Vec<u8>,
) -> Result<CatalogDurablePublicationEntry, CatalogContractError> {
    if key.iter().all(|byte| *byte == 0) || payload.is_empty() {
        return Err(CatalogContractError::invalid(
            "catalog durable publication entries require nonzero keys and nonempty payloads",
        ));
    }
    if payload.len() > MAX_DURABLE_PUBLICATION_BYTES {
        return Err(CatalogContractError::invalid(
            "one catalog durable publication entry exceeds the aggregate byte ceiling",
        ));
    }
    Ok(CatalogDurablePublicationEntry {
        kind,
        key,
        payload_digest: *blake3::hash(&payload).as_bytes(),
        payload,
    })
}

fn derive_member_binding_frame_key(
    binding: &CatalogPublicationMemberBinding,
) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-durable-member-binding-v1\0");
    hash_plan_source(&mut hasher, &binding.source);
    hasher.update(binding.member_ref.as_bytes());
    *hasher.finalize().as_bytes()
}

pub(crate) fn derive_durable_entries_digest(
    entries: &[(
        CatalogDurablePublicationEntryKind,
        [u8; DIGEST_BYTES],
        usize,
        [u8; DIGEST_BYTES],
    )],
) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-durable-entries-v1\0");
    hasher.update(&(entries.len() as u64).to_be_bytes());
    for (kind, key, payload_len, payload_digest) in entries {
        hash_component(&mut hasher, kind.as_str().as_bytes());
        hasher.update(key);
        hasher.update(&(*payload_len as u64).to_be_bytes());
        hasher.update(payload_digest);
    }
    *hasher.finalize().as_bytes()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn derive_durable_content_digest(
    build: CatalogInitialBuildExpectation,
    contract_selection_json: &[u8],
    member_identity_contract_id: Option<&str>,
    publication_digest: CatalogInitialPublicationDigest,
    reducer_revision: super::evidence::CatalogReducerPublicationRevision,
    source_count: usize,
    member_count: usize,
    project_row_count: usize,
    session_row_count: usize,
    tombstone_count: usize,
    encoded_bytes: usize,
    entries_digest: [u8; DIGEST_BYTES],
) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-durable-content-v1\0");
    hasher.update(&CATALOG_DURABLE_PUBLICATION_CONTRACT_VERSION.to_be_bytes());
    hasher.update(build.coverage_plan_id.storage_bytes());
    hasher.update(&build.desired_contract_version.to_be_bytes());
    hasher.update(&build.epoch.to_be_bytes());
    hasher.update(&build.attempt.to_be_bytes());
    hash_component(&mut hasher, contract_selection_json);
    match member_identity_contract_id {
        Some(value) => {
            hasher.update(&[1]);
            hash_component(&mut hasher, value.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(publication_digest.storage_bytes());
    hasher.update(reducer_revision.storage_bytes());
    for value in [
        source_count,
        member_count,
        project_row_count,
        session_row_count,
        tombstone_count,
        encoded_bytes,
    ] {
        hasher.update(&(value as u64).to_be_bytes());
    }
    hasher.update(&entries_digest);
    *hasher.finalize().as_bytes()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn derive_durable_refresh_content_digest(
    build: CatalogRefreshBuildExpectation,
    predecessor: &CatalogRefreshPredecessor,
    contract_selection_json: &[u8],
    member_identity_contract_id: Option<&str>,
    publication_digest: CatalogRefreshPublicationDigest,
    reducer_revision: super::evidence::CatalogReducerPublicationRevision,
    member_history_revision: CatalogMemberHistoryRevision,
    source_count: usize,
    member_count: usize,
    project_row_count: usize,
    session_row_count: usize,
    tombstone_count: usize,
    encoded_bytes: usize,
    entries_digest: [u8; DIGEST_BYTES],
) -> [u8; DIGEST_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-durable-content-v2\0");
    hasher.update(&CATALOG_DURABLE_REFRESH_PUBLICATION_CONTRACT_VERSION.to_be_bytes());
    hasher.update(build.coverage_plan_id.storage_bytes());
    hasher.update(&build.desired_contract_version.to_be_bytes());
    hasher.update(&build.epoch.to_be_bytes());
    hasher.update(&build.attempt.to_be_bytes());
    hasher.update(&build.refresh_started_commit_seq.to_be_bytes());
    hash_snapshot_id(&mut hasher, build.predecessor_snapshot);
    if build.plan_replacement {
        hasher.update(b"spaghetti/rfc012b/catalog-coverage-plan-replacement-v1\0");
    }
    hasher.update(predecessor.publication_digest());
    hasher.update(predecessor.content_digest());
    hasher.update(predecessor.reducer_revision().storage_bytes());
    hasher.update(predecessor.member_history_revision().as_bytes());
    hash_component(&mut hasher, contract_selection_json);
    match member_identity_contract_id {
        Some(value) => {
            hasher.update(&[1]);
            hash_component(&mut hasher, value.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(publication_digest.storage_bytes());
    hasher.update(reducer_revision.storage_bytes());
    hasher.update(member_history_revision.as_bytes());
    for value in [
        source_count,
        member_count,
        project_row_count,
        session_row_count,
        tombstone_count,
        encoded_bytes,
    ] {
        hasher.update(&(value as u64).to_be_bytes());
    }
    hasher.update(&entries_digest);
    *hasher.finalize().as_bytes()
}

pub(crate) fn validate_durable_contract_selection(
    selection: &ContractVersionSelection,
) -> Result<(), CatalogContractError> {
    validate_contract_selection(selection)
}

pub(crate) fn validate_durable_source_coverage(
    plan_source: &CatalogCoveragePlanSource,
    selection: &ContractVersionSelection,
    coverage: &SourceCoverageSet,
) -> Result<(), CatalogContractError> {
    validate_complete_source_coverage(plan_source, selection, coverage)
}

fn validate_contract_selection(
    selection: &ContractVersionSelection,
) -> Result<(), CatalogContractError> {
    if selection.selection_contract_version != CONTRACT_VERSION_SELECTION_VERSION
        || selection.model_major != CATALOG_BASE_MODEL_MAJOR
        || selection.external_entity_reference_version != EXTERNAL_ENTITY_REFERENCE_VERSION
        || selection.semantic_revision_reference_version != SEMANTIC_REFERENCE_CONTRACT_VERSION
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
    let active_initial = (readiness.state == CatalogReadinessPhase::Building
        && readiness.source_coverage.is_empty())
        || (readiness.state == CatalogReadinessPhase::Partial
            && !readiness.source_coverage.is_empty());
    if plan.scope != CatalogCoverageScope::Library
        || !active_initial
        || readiness.desired_contract_version != CATALOG_QUERY_PACK_CONTRACT_VERSION
        || selection.query_pack_version != Some(readiness.desired_contract_version)
        || readiness.completed_contract_version.is_some()
        || readiness.complete_through_commit.is_some()
        || readiness.last_complete_snapshot.is_some()
        || readiness.refreshing_from_snapshot.is_some()
        || !matches!(
            readiness.reason.as_ref(),
            None | Some(CatalogReadinessReason::SourceRetrying { .. })
        )
    {
        return Err(CatalogContractError::invalid(
            "initial catalog publication requires one exact durable Library Building/Partial expectation",
        ));
    }
    Ok(CatalogInitialBuildExpectation {
        coverage_plan_id: plan.coverage_plan_id,
        desired_contract_version: readiness.desired_contract_version,
        epoch: readiness.epoch,
        attempt: readiness.attempt,
    })
}

fn validate_refresh_build(
    plan: &CatalogCoveragePlan,
    readiness: &CatalogReadinessSnapshot,
    refresh_started_commit_seq: u64,
    predecessor: &CatalogRefreshPredecessor,
    selection: &ContractVersionSelection,
) -> Result<CatalogRefreshBuildExpectation, CatalogContractError> {
    plan.validate()?;
    readiness.validate_against(plan)?;
    validate_contract_selection(selection)?;
    let snapshot = predecessor.snapshot_id;
    let active_ready = readiness.state == CatalogReadinessPhase::Ready
        && readiness.complete_through_commit == Some(snapshot.complete_commit)
        && readiness.refreshing_from_snapshot == Some(snapshot)
        && matches!(
            readiness.reason.as_ref(),
            None | Some(CatalogReadinessReason::SourceRetrying { .. })
        );
    let recovery_building = matches!(
        readiness.state,
        CatalogReadinessPhase::Building | CatalogReadinessPhase::Partial
    ) && readiness.complete_through_commit.is_none()
        && readiness.refreshing_from_snapshot.is_none()
        && matches!(
            readiness.reason.as_ref(),
            None | Some(CatalogReadinessReason::SourceRetrying { .. })
        )
        && ((snapshot.readiness_epoch == readiness.epoch && readiness.attempt > 1)
            || snapshot.readiness_epoch < readiness.epoch);
    let plan_lineage_is_exact = if predecessor.is_plan_replacement() {
        snapshot.coverage_plan_id != plan.coverage_plan_id
            && recovery_building
            && snapshot.readiness_epoch < readiness.epoch
    } else {
        snapshot.coverage_plan_id == plan.coverage_plan_id
    };
    if plan.scope != CatalogCoverageScope::Library
        || (!active_ready && !recovery_building)
        || readiness.coverage_plan_id != plan.coverage_plan_id
        || readiness.desired_contract_version != CATALOG_QUERY_PACK_CONTRACT_VERSION
        || readiness.completed_contract_version != Some(readiness.desired_contract_version)
        || readiness.last_complete_snapshot != Some(snapshot)
        || refresh_started_commit_seq <= snapshot.complete_commit
        || selection != &predecessor.contract_selection
        || selection.query_pack_version != Some(readiness.desired_contract_version)
        || !plan_lineage_is_exact
        || snapshot.pack_contract_version != readiness.desired_contract_version
        || snapshot.readiness_epoch > readiness.epoch
    {
        return Err(CatalogContractError::invalid(
            "catalog refresh publication requires one exact active or degraded-recovery lineage",
        ));
    }
    Ok(CatalogRefreshBuildExpectation {
        coverage_plan_id: plan.coverage_plan_id,
        desired_contract_version: readiness.desired_contract_version,
        epoch: readiness.epoch,
        attempt: readiness.attempt,
        refresh_started_commit_seq,
        predecessor_snapshot: snapshot,
        plan_replacement: predecessor.is_plan_replacement(),
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

pub(crate) fn validate_restarted_initial_publication(
    plan: &CatalogCoveragePlan,
    selection: &ContractVersionSelection,
    expected_member_identity_contract_id: Option<&str>,
    source_frames: Vec<CatalogDurableSourceFrame>,
    mut member_bindings: Vec<CatalogPublicationMemberBinding>,
    reducer: &CatalogReducerPublication,
) -> Result<Vec<SourceCoverageSet>, CatalogContractError> {
    plan.validate()?;
    validate_contract_selection(selection)?;
    if plan.scope != CatalogCoverageScope::Library {
        return Err(CatalogContractError::invalid(
            "durable catalog restart requires a Library coverage plan",
        ));
    }
    let planned_source_count = plan
        .required_sources
        .len()
        .checked_add(plan.optional_sources.len())
        .ok_or_else(|| CatalogContractError::invalid("catalog source count overflow"))?;
    if source_frames.len() > planned_source_count {
        return Err(CatalogContractError::invalid(
            "durable catalog restart contains more sources than its plan",
        ));
    }

    let mut member_refs_by_source = BTreeMap::new();
    for binding in &member_bindings {
        member_refs_by_source
            .entry(binding.source.clone())
            .or_insert_with(Vec::new)
            .push(binding.member_ref);
    }
    let mut sources = Vec::with_capacity(source_frames.len());
    for frame in source_frames {
        if frame.contract_selection != *selection
            || expected_member_identity_contract_id
                != Some(frame.member_identity_contract_id.as_str())
        {
            return Err(CatalogContractError::invalid(
                "durable catalog source differs from the snapshot selection or identity contract",
            ));
        }
        let member_refs = member_refs_by_source
            .remove(&frame.plan_source)
            .unwrap_or_default();
        sources.push(frame.complete(member_refs)?);
    }
    if !member_refs_by_source.is_empty() {
        return Err(CatalogContractError::invalid(
            "durable catalog member binding names a source without a complete source frame",
        ));
    }
    sources.sort_by(|left, right| left.plan_source.cmp(&right.plan_source));
    if sources
        .windows(2)
        .any(|pair| pair[0].plan_source == pair[1].plan_source)
        || expected_member_identity_contract_id.is_some() != !sources.is_empty()
    {
        return Err(CatalogContractError::invalid(
            "durable catalog restart source identities are duplicate or inconsistent",
        ));
    }
    validate_plan_sources(plan, selection, &sources)?;
    validate_covered_reducer(&sources, reducer)?;
    member_bindings.sort_by(|left, right| left.coordinate().cmp(&right.coordinate()));
    validate_member_bindings(&sources, reducer, &member_bindings)?;
    Ok(sources
        .into_iter()
        .map(|source| source.source_coverage)
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_restarted_refresh_publication(
    plan: &CatalogCoveragePlan,
    selection: &ContractVersionSelection,
    expected_member_identity_contract_id: Option<&str>,
    source_frames: Vec<CatalogDurableSourceFrame>,
    member_bindings: Vec<CatalogPublicationMemberBinding>,
    member_history: &CatalogPublicationMemberHistory,
    reducer: &CatalogReducerPublication,
    prior_member_history: &CatalogPublicationMemberHistory,
    prior_reducer: &CatalogReducerPublication,
) -> Result<Vec<SourceCoverageSet>, CatalogContractError> {
    prior_reducer.validate_refresh_successor(reducer)?;
    let expected_history = prior_member_history.successor(&member_bindings)?;
    if &expected_history != member_history {
        return Err(CatalogContractError::invalid(
            "durable catalog refresh member history is not the exact cumulative successor",
        ));
    }
    validate_restarted_initial_publication(
        plan,
        selection,
        expected_member_identity_contract_id,
        source_frames,
        member_bindings,
        reducer,
    )
}

fn hash_component(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn derive_member_history_revision(
    entries: &[CatalogPublicationMemberHistoryEntry],
) -> CatalogMemberHistoryRevision {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-member-history-v1\0");
    hasher.update(&(entries.len() as u64).to_be_bytes());
    for entry in entries {
        hasher.update(entry.member_ref.as_bytes());
        hasher.update(
            &entry
                .session_ref
                .external_ref
                .external_entity_reference_version
                .to_be_bytes(),
        );
        hasher.update(entry.session_ref.external_ref.entity_key.as_bytes());
    }
    CatalogMemberHistoryRevision::from_digest(*hasher.finalize().as_bytes())
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

#[allow(clippy::too_many_arguments)]
fn derive_refresh_publication_digest(
    build: CatalogRefreshBuildExpectation,
    predecessor: &CatalogRefreshPredecessor,
    selection: &ContractVersionSelection,
    member_identity_contract_id: Option<&str>,
    sources: &[CatalogCompleteSourceAssembly],
    bindings: &[CatalogPublicationMemberBinding],
    member_history: &CatalogPublicationMemberHistory,
    reducer: &CatalogReducerPublication,
    limits: CatalogPublicationLimits,
) -> CatalogRefreshPublicationDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti/rfc012b/catalog-refresh-publication-v1\0");
    hasher.update(&CATALOG_REFRESH_PUBLICATION_CONTRACT_VERSION.to_be_bytes());
    hasher.update(build.coverage_plan_id.storage_bytes());
    hasher.update(&build.desired_contract_version.to_be_bytes());
    hasher.update(&build.epoch.to_be_bytes());
    hasher.update(&build.attempt.to_be_bytes());
    hasher.update(&build.refresh_started_commit_seq.to_be_bytes());
    hash_snapshot_id(&mut hasher, build.predecessor_snapshot);
    if build.plan_replacement {
        hasher.update(b"spaghetti/rfc012b/catalog-coverage-plan-replacement-v1\0");
    }
    hasher.update(&predecessor.publication_digest);
    hasher.update(&predecessor.content_digest);
    hasher.update(predecessor.reducer_revision.storage_bytes());
    hasher.update(predecessor.member_history_revision.as_bytes());
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
        hasher.update(binding.session_ref.external_ref.entity_key.as_bytes());
    }
    hasher.update(member_history.revision.as_bytes());
    hasher.update(reducer.revision().as_bytes());
    hasher.update(&(reducer.project_row_count() as u64).to_be_bytes());
    hasher.update(&(reducer.session_row_count() as u64).to_be_bytes());
    hasher.update(&(reducer.tombstone_count() as u64).to_be_bytes());
    CatalogRefreshPublicationDigest::from_digest(*hasher.finalize().as_bytes())
}

fn hash_snapshot_id(hasher: &mut blake3::Hasher, snapshot: CatalogSnapshotId) {
    hasher.update(&snapshot.pack_contract_version.to_be_bytes());
    hasher.update(snapshot.coverage_plan_id.storage_bytes());
    hasher.update(&snapshot.readiness_epoch.to_be_bytes());
    hasher.update(&snapshot.complete_commit.to_be_bytes());
}

#[cfg(test)]
mod tests;
