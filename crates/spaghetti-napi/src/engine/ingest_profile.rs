//! Throwaway wall-clock isolation for ingest.
//!
//! Set `SPAGHETTI_INGEST_PROFILE_SKIP=facts,messages,runtime,...` to no-op one
//! stage. Production leaves the variable unset. These flags exist only to rank
//! critical-path cost by subtraction; they must not become product settings.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IngestProfileSkip {
    pub checkpoints: bool,
    pub finalize: bool,
    pub facts: bool,
    pub messages: bool,
    pub runtime: bool,
    pub delegation: bool,
    pub artifact: bool,
    pub artifact_reductions: bool,
    pub artifact_reduction_deferral: bool,
    pub delegation_reductions: bool,
    pub activity_evidence_ownership: bool,
    pub bootstrap_integrity_deferral: bool,
    pub usage: bool,
    pub extras: bool,
}

impl IngestProfileSkip {
    pub(crate) fn current() -> Self {
        static SKIP: OnceLock<IngestProfileSkip> = OnceLock::new();
        *SKIP.get_or_init(Self::from_env)
    }

    pub(crate) fn relaxes_sqlite_constraints(self) -> bool {
        self.facts
            || self.messages
            || self.runtime
            || self.delegation
            || self.artifact
            || self.artifact_reductions
            || self.delegation_reductions
            || self.usage
            || self.extras
    }

    fn from_env() -> Self {
        Self::from_tokens(&std::env::var("SPAGHETTI_INGEST_PROFILE_SKIP").unwrap_or_default())
    }

    fn from_tokens(raw: &str) -> Self {
        let mut skip = Self::default();
        for token in raw
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            match token {
                "checkpoints" => skip.checkpoints = true,
                "finalize" => skip.finalize = true,
                "facts" => skip.facts = true,
                "messages" => skip.messages = true,
                "runtime" => skip.runtime = true,
                "delegation" => skip.delegation = true,
                "artifact" => skip.artifact = true,
                "artifact-reductions" => skip.artifact_reductions = true,
                "artifact-reduction-deferral" => skip.artifact_reduction_deferral = true,
                "delegation-reductions" => skip.delegation_reductions = true,
                "activity-evidence-ownership" => skip.activity_evidence_ownership = true,
                "bootstrap-integrity-deferral" => skip.bootstrap_integrity_deferral = true,
                "usage" => skip.usage = true,
                "extras" => skip.extras = true,
                "all-writes" => {
                    skip.facts = true;
                    skip.messages = true;
                    skip.runtime = true;
                    skip.delegation = true;
                    skip.artifact = true;
                    skip.artifact_reductions = true;
                    skip.delegation_reductions = true;
                    skip.usage = true;
                    skip.extras = true;
                }
                other => {
                    eprintln!("spaghetti ingest profile: ignoring unknown skip {other}");
                }
            }
        }
        skip
    }
}

#[cfg(test)]
mod tests {
    use super::IngestProfileSkip;

    #[test]
    fn empty_tokens_skip_nothing() {
        assert_eq!(
            IngestProfileSkip::from_tokens(""),
            IngestProfileSkip::default()
        );
    }

    #[test]
    fn all_writes_covers_projection_tables() {
        let skip = IngestProfileSkip::from_tokens("all-writes");
        assert!(skip.facts && skip.messages && skip.runtime && skip.delegation);
        assert!(skip.artifact && skip.artifact_reductions);
        assert!(skip.delegation_reductions && skip.usage && skip.extras);
        assert!(!skip.activity_evidence_ownership);
        assert!(!skip.artifact_reduction_deferral);
        assert!(!skip.bootstrap_integrity_deferral);
        assert!(!skip.checkpoints && !skip.finalize);
        assert!(skip.relaxes_sqlite_constraints());
    }

    #[test]
    fn activity_evidence_ownership_is_an_isolated_control_switch() {
        let skip = IngestProfileSkip::from_tokens("activity-evidence-ownership");
        assert!(skip.activity_evidence_ownership);
        assert!(!skip.relaxes_sqlite_constraints());
    }

    #[test]
    fn artifact_reduction_deferral_is_an_isolated_control_switch() {
        let skip = IngestProfileSkip::from_tokens("artifact-reduction-deferral");
        assert!(skip.artifact_reduction_deferral);
        assert!(!skip.relaxes_sqlite_constraints());
    }

    #[test]
    fn bootstrap_integrity_deferral_is_an_isolated_control_switch() {
        let skip = IngestProfileSkip::from_tokens("bootstrap-integrity-deferral");
        assert!(skip.bootstrap_integrity_deferral);
        assert!(!skip.relaxes_sqlite_constraints());
    }
}
