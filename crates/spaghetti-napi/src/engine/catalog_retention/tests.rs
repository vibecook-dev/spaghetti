use std::collections::BTreeMap;

use rusqlite::{params, Connection};

use super::*;
use crate::adapter::{
    ContractVersionSelection, CONTRACT_VERSION_SELECTION_VERSION, SOURCE_COVERAGE_CONTRACT_VERSION,
};
use crate::catalog_contract::{CatalogCoveragePlan, CATALOG_QUERY_PACK_CONTRACT_VERSION};
use crate::core::schema;

fn selection() -> ContractVersionSelection {
    ContractVersionSelection {
        selection_contract_version: CONTRACT_VERSION_SELECTION_VERSION,
        model_major: 1,
        external_entity_reference_version: 1,
        semantic_revision_reference_version: 1,
        coverage_contract_version: SOURCE_COVERAGE_CONTRACT_VERSION,
        fact_family_versions: BTreeMap::from([
            ("catalog.project".to_owned(), 1),
            ("catalog.session".to_owned(), 1),
        ]),
        query_pack_version: Some(CATALOG_QUERY_PACK_CONTRACT_VERSION),
        observation_contract_version: None,
    }
}

fn expectation() -> CatalogSnapshotRetirementExpectation {
    let plan =
        CatalogCoveragePlan::new(CatalogCoverageScope::Library, Vec::new(), Vec::new()).unwrap();
    let predecessor = CatalogSnapshotId::new(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        plan.coverage_plan_id,
        1,
        10,
    )
    .unwrap();
    let successor = CatalogSnapshotId::new(
        CATALOG_QUERY_PACK_CONTRACT_VERSION,
        plan.coverage_plan_id,
        1,
        20,
    )
    .unwrap();
    CatalogSnapshotRetirementExpectation::new(
        CatalogCoverageScope::Library,
        plan.coverage_plan_id,
        selection(),
        1,
        1,
        successor.complete_commit,
        0,
        CatalogRetainedSnapshotCommitment::from_test_parts(
            predecessor,
            [1; DIGEST_BYTES],
            [2; DIGEST_BYTES],
        ),
        CatalogRetainedSnapshotCommitment::from_test_parts(
            successor,
            [3; DIGEST_BYTES],
            [4; DIGEST_BYTES],
        ),
    )
    .unwrap()
}

#[test]
fn retirement_expectation_is_exact_bounded_and_redacted() {
    let expected = expectation();
    let debug = format!("{expected:?}");
    assert!(debug.contains("target_snapshot"));
    assert!(debug.contains("successor_snapshot"));
    assert!(!debug.contains("contract_selection"));
    assert!(!debug.contains(&"01".repeat(DIGEST_BYTES)));
    let commitment_debug = format!("{:?}", expected.target());
    assert!(!commitment_debug.contains("publication_digest"));
    assert!(!commitment_debug.contains("content_digest"));

    let mut zero_epoch = expected.clone();
    zero_epoch.epoch = 0;
    assert!(zero_epoch.validate().is_err());

    let mut stale_successor = expected.clone();
    stale_successor.state_commit_seq += 1;
    assert!(stale_successor.validate().is_err());

    let mut exhausted = expected.clone();
    exhausted.retired_prefix_len = MAX_RETAINED_REFRESH_LINEAGE_DEPTH;
    assert!(exhausted.validate().is_err());

    let mut drifted_selection = expected.clone();
    drifted_selection.contract_selection.query_pack_version = Some(2);
    assert!(drifted_selection.validate().is_err());

    let mut zero_commitment = expected;
    let target = zero_commitment.target();
    zero_commitment.target = CatalogRetainedSnapshotCommitment::from_test_parts(
        target.snapshot_id(),
        [0; DIGEST_BYTES],
        target.content_digest(),
    );
    assert!(zero_commitment.validate().is_err());
}

#[test]
fn retirement_command_rejects_reversed_time_and_redacts_digests() {
    assert!(CatalogSnapshotRetirementCommand::new(expectation(), 20, 19).is_err());
    let command = CatalogSnapshotRetirementCommand::new(expectation(), 20, 21).unwrap();
    let debug = format!("{command:?}");
    assert!(debug.contains("started_at"));
    assert!(!debug.contains(&"01".repeat(DIGEST_BYTES)));
}

#[test]
fn retirement_restart_scan_accepts_the_exact_bound_and_stops_at_one_over() {
    let plan =
        CatalogCoveragePlan::new(CatalogCoverageScope::Library, Vec::new(), Vec::new()).unwrap();
    let chain = (0..=MAX_RETAINED_REFRESH_LINEAGE_DEPTH)
        .map(|index| {
            let snapshot = CatalogSnapshotId::new(
                CATALOG_QUERY_PACK_CONTRACT_VERSION,
                plan.coverage_plan_id,
                1,
                10 + u64::try_from(index).unwrap(),
            )
            .unwrap();
            CatalogRetainedSnapshotCommitment::from_test_parts(
                snapshot,
                [u8::try_from(index + 1).unwrap(); DIGEST_BYTES],
                [u8::try_from(index + 33).unwrap(); DIGEST_BYTES],
            )
        })
        .collect::<Vec<_>>();
    let current = *chain.last().unwrap();
    let connection = Connection::open_in_memory().unwrap();
    schema::initialize_schema(&connection).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();

    for (index, target) in chain
        .iter()
        .copied()
        .take(MAX_RETAINED_REFRESH_LINEAGE_DEPTH)
        .enumerate()
    {
        let retirement_commit = 100 + i64::try_from(index).unwrap();
        connection
            .execute(
                "INSERT INTO ingest_commits (commit_seq, source_instance_id, reason, started_at, committed_at, fact_count) VALUES (?1, NULL, ?2, ?3, ?3, 0)",
                params![retirement_commit, RETIREMENT_REASON, retirement_commit],
            )
            .unwrap();
        connection
            .execute(
                r#"
                INSERT INTO catalog_snapshot_retirements (
                    snapshot_commit_seq, snapshot_publication_digest,
                    snapshot_content_digest, successor_snapshot_commit_seq,
                    successor_publication_digest, successor_content_digest,
                    retirement_commit_seq, retired_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                "#,
                params![
                    i64::try_from(target.snapshot_id().complete_commit).unwrap(),
                    target.publication_digest().as_slice(),
                    target.content_digest().as_slice(),
                    i64::try_from(current.snapshot_id().complete_commit).unwrap(),
                    current.publication_digest().as_slice(),
                    current.content_digest().as_slice(),
                    retirement_commit,
                ],
            )
            .unwrap();
    }
    assert_eq!(
        load_retired_prefix(&connection, &chain).unwrap(),
        MAX_RETAINED_REFRESH_LINEAGE_DEPTH
    );

    connection
        .execute_batch("DROP TRIGGER catalog_snapshot_retirements_no_update;")
        .unwrap();
    let stale_successor = chain[1];
    connection
        .execute(
            r#"
            UPDATE catalog_snapshot_retirements
            SET successor_snapshot_commit_seq = ?1,
                successor_publication_digest = ?2,
                successor_content_digest = ?3
            WHERE snapshot_commit_seq = ?4
            "#,
            params![
                i64::try_from(stale_successor.snapshot_id().complete_commit).unwrap(),
                stale_successor.publication_digest().as_slice(),
                stale_successor.content_digest().as_slice(),
                i64::try_from(chain[0].snapshot_id().complete_commit).unwrap(),
            ],
        )
        .unwrap();
    let error = load_retired_prefix(&connection, &chain).unwrap_err();
    assert!(
        error.to_string().contains("exact current snapshot"),
        "unexpected stale-successor error: {error}"
    );
    connection
        .execute(
            r#"
            UPDATE catalog_snapshot_retirements
            SET successor_snapshot_commit_seq = ?1,
                successor_publication_digest = ?2,
                successor_content_digest = ?3
            WHERE snapshot_commit_seq = ?4
            "#,
            params![
                i64::try_from(current.snapshot_id().complete_commit).unwrap(),
                current.publication_digest().as_slice(),
                current.content_digest().as_slice(),
                i64::try_from(chain[0].snapshot_id().complete_commit).unwrap(),
            ],
        )
        .unwrap();

    let extra_commit = 200_i64;
    connection
        .execute(
            "INSERT INTO ingest_commits (commit_seq, source_instance_id, reason, started_at, committed_at, fact_count) VALUES (?1, NULL, ?2, ?1, ?1, 0)",
            params![extra_commit, RETIREMENT_REASON],
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO catalog_snapshot_retirements (
                snapshot_commit_seq, snapshot_publication_digest,
                snapshot_content_digest, successor_snapshot_commit_seq,
                successor_publication_digest, successor_content_digest,
                retirement_commit_seq, retired_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            "#,
            params![
                i64::try_from(current.snapshot_id().complete_commit).unwrap(),
                current.publication_digest().as_slice(),
                current.content_digest().as_slice(),
                i64::try_from(current.snapshot_id().complete_commit + 1).unwrap(),
                [91_u8; DIGEST_BYTES].as_slice(),
                [92_u8; DIGEST_BYTES].as_slice(),
                extra_commit,
            ],
        )
        .unwrap();
    let error = load_retired_prefix(&connection, &chain).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("exceeds the bounded non-current ancestry"),
        "unexpected error: {error}"
    );
}
