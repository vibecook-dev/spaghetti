//! Interning for RFC 012C usage-v2 qualification specs.
//!
//! Every response-level usage fact carries six qualified values (four token
//! buckets plus model and effort). Their qualification metadata repeats across
//! the whole corpus, so it is interned into one row per distinct spec and
//! referenced by digest from `usage_v2_response_contributions`.

use rusqlite::{params, Transaction};

use crate::adapter::{
    ContractCompleteness, QualifiedUnknownReason, QualifiedValueQuality, UsageQualifiedValue,
    UsageResponseIdentity, UsageValueAuthority,
};

use super::projection::{execute_cached, sqlite_error};
use super::EngineError;

/// Intern one qualification spec, returning 1 affected row when the stored row
/// already carries identical content and 0 when a digest collision would change
/// it.
///
/// The `DO UPDATE` target must never be `qualification_key`. Six columns of
/// `usage_v2_response_contributions` reference that key and none of them is
/// indexed, so an upsert that assigns the parent key makes SQLite prove no
/// child still references the old value — a full scan of the contributions
/// table per referencing constraint, on every intern, against a table that
/// grows with the corpus. Assigning a non-key column instead leaves the parent
/// key unmodified, so no foreign-key enforcement is generated at all, and the
/// row content is identical either way because the `WHERE` clause admits the
/// update only when every column already matches.
const USAGE_V2_QUALIFICATION_UPSERT_SQL: &str = r#"
    INSERT INTO usage_v2_qualification_specs (
        qualification_key, quality, completeness, unknown_reason,
        authority, native_field, normalization_contract_version
    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
    ON CONFLICT(qualification_key) DO UPDATE SET
        normalization_contract_version = excluded.normalization_contract_version
    WHERE quality = excluded.quality
      AND completeness = excluded.completeness
      AND unknown_reason IS excluded.unknown_reason
      AND authority = excluded.authority
      AND native_field = excluded.native_field
      AND normalization_contract_version = excluded.normalization_contract_version
"#;

pub(super) fn intern_usage_v2_qualification<T>(
    transaction: &Transaction<'_>,
    value: &UsageQualifiedValue<T>,
) -> Result<[u8; 32], EngineError> {
    let key = usage_v2_qualification_key(value);
    let affected = execute_cached(
        transaction,
        USAGE_V2_QUALIFICATION_UPSERT_SQL,
        params![
            key.as_slice(),
            usage_v2_quality(value.quality),
            usage_v2_completeness(value.completeness),
            value.unknown_reason.map(usage_v2_unknown_reason),
            usage_v2_authority(value.authority),
            value.provenance.native_field,
            i64::from(value.provenance.normalization_contract_version),
        ],
    )
    .map_err(|error| sqlite_error("intern usage-v2 qualification", error))?;
    if affected != 1 {
        return Err(EngineError::InvalidCommit(
            "usage-v2 qualification digest collision".to_string(),
        ));
    }
    Ok(key)
}

pub(super) fn usage_v2_qualification_key<T>(value: &UsageQualifiedValue<T>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"spaghetti-usage-v2-qualification-v1\0");
    for component in [
        usage_v2_quality(value.quality).as_bytes(),
        usage_v2_completeness(value.completeness).as_bytes(),
        value
            .unknown_reason
            .map(usage_v2_unknown_reason)
            .unwrap_or("")
            .as_bytes(),
        usage_v2_authority(value.authority).as_bytes(),
        value.provenance.native_field.as_bytes(),
    ] {
        hasher.update(&(component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    hasher.update(
        &value
            .provenance
            .normalization_contract_version
            .to_be_bytes(),
    );
    *hasher.finalize().as_bytes()
}

pub(super) fn usage_v2_response_identity(identity: UsageResponseIdentity) -> &'static str {
    match identity {
        UsageResponseIdentity::NativeMessageId => "native_message_id",
        UsageResponseIdentity::SourceRecordFallback => "source_record_fallback",
    }
}

pub(super) fn usage_v2_quality(quality: QualifiedValueQuality) -> &'static str {
    match quality {
        QualifiedValueQuality::Exact => "exact",
        QualifiedValueQuality::NativeClaimed => "native_claimed",
        QualifiedValueQuality::Derived => "derived",
        QualifiedValueQuality::Estimated => "estimated",
        QualifiedValueQuality::Unknown => "unknown",
    }
}

pub(super) fn usage_v2_completeness(completeness: ContractCompleteness) -> &'static str {
    match completeness {
        ContractCompleteness::Complete => "complete",
        ContractCompleteness::Partial => "partial",
        ContractCompleteness::Unknown => "unknown",
    }
}

pub(super) fn usage_v2_unknown_reason(reason: QualifiedUnknownReason) -> &'static str {
    match reason {
        QualifiedUnknownReason::Missing => "missing",
        QualifiedUnknownReason::Unsupported => "unsupported",
        QualifiedUnknownReason::Withheld => "withheld",
        QualifiedUnknownReason::NotYetObserved => "not_yet_observed",
        QualifiedUnknownReason::Ambiguous => "ambiguous",
        QualifiedUnknownReason::Malformed => "malformed",
    }
}

pub(super) fn usage_v2_authority(authority: UsageValueAuthority) -> &'static str {
    match authority {
        UsageValueAuthority::NativeResponse => "native_response",
        UsageValueAuthority::AdapterDerived => "adapter_derived",
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::{params, Connection, StatementStatus};

    use super::USAGE_V2_QUALIFICATION_UPSERT_SQL;
    use crate::core::schema;

    /// Seed one qualification spec and `rows` contributions that all reference
    /// it. Foreign keys are disabled only while seeding, so the rows do not
    /// need real `fact_records` parents; the declarations that matter to this
    /// test are on the table, not on the data.
    fn seed(rows: usize) -> Connection {
        let connection = Connection::open_in_memory().expect("open in-memory database");
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .expect("disable foreign keys while seeding");
        schema::initialize_schema(&connection).expect("initialize schema");
        let key = [7_u8; 32];
        connection
            .execute(
                "INSERT INTO usage_v2_qualification_specs (
                     qualification_key, quality, completeness, unknown_reason,
                     authority, native_field, normalization_contract_version
                 ) VALUES (?1, 'exact', 'complete', NULL, 'native_response', 'usage.input_tokens', 1)",
                params![key.as_slice()],
            )
            .expect("insert qualification spec");
        for row in 0..rows {
            let mut identity = [0_u8; 32];
            identity[..8].copy_from_slice(&(row as u64).to_be_bytes());
            connection
                .execute(
                    "INSERT INTO usage_v2_response_contributions (
                         usage_key, fact_revision_id, source_record_id, fact_id,
                         session_key, actor_run_key, response_key, response_identity,
                         input_tokens, input_qualification_key,
                         output_tokens, output_qualification_key,
                         cache_creation_input_tokens, cache_creation_qualification_key,
                         cache_read_input_tokens, cache_read_qualification_key,
                         source_object_id, source_generation, cursor_end, last_commit_seq
                     ) VALUES (?1, ?1, ?1, ?1, ?1, ?1, ?1, 'source_record_fallback',
                               1, ?2, 1, ?2, 0, ?2, 0, ?2, 1, 1, ?1, 1)",
                    params![identity.as_slice(), key.as_slice()],
                )
                .expect("insert contribution row");
        }
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("enable foreign keys for the measured upsert");
        connection
    }

    /// Re-interning an existing spec must not depend on how many contributions
    /// already reference it. Assigning the conflict key in `DO UPDATE` makes
    /// SQLite re-check six unindexed child columns, which is a full scan of the
    /// contributions table per constraint on every usage fact.
    #[test]
    fn re_interning_a_spec_never_scans_the_contributions_that_reference_it() {
        for rows in [0_usize, 64, 4_096] {
            let connection = seed(rows);
            let mut statement = connection
                .prepare_cached(USAGE_V2_QUALIFICATION_UPSERT_SQL)
                .expect("prepare the interning upsert");
            let affected = statement
                .execute(params![
                    [7_u8; 32].as_slice(),
                    "exact",
                    "complete",
                    None::<&str>,
                    "native_response",
                    "usage.input_tokens",
                    1_i64,
                ])
                .expect("re-intern an identical spec");
            assert_eq!(affected, 1, "re-interning reports the row it kept");
            assert_eq!(
                statement.get_status(StatementStatus::FullscanStep),
                0,
                "re-interning scanned rows with {rows} contributions present"
            );
        }
    }

    /// The upsert is how a digest collision is detected: identical content
    /// keeps the row, and differing content under the same key changes nothing
    /// so the caller can reject the commit.
    #[test]
    fn a_differing_spec_under_the_same_key_updates_nothing() {
        let connection = seed(8);
        let affected = connection
            .prepare_cached(USAGE_V2_QUALIFICATION_UPSERT_SQL)
            .expect("prepare the interning upsert")
            .execute(params![
                [7_u8; 32].as_slice(),
                "derived",
                "complete",
                None::<&str>,
                "native_response",
                "usage.input_tokens",
                1_i64,
            ])
            .expect("attempt a colliding spec");
        assert_eq!(
            affected, 0,
            "a colliding spec must not overwrite the stored row"
        );
        let quality: String = connection
            .query_row(
                "SELECT quality FROM usage_v2_qualification_specs WHERE qualification_key = ?1",
                params![[7_u8; 32].as_slice()],
                |row| row.get(0),
            )
            .expect("read the stored spec");
        assert_eq!(quality, "exact", "the stored spec is unchanged");
    }
}
