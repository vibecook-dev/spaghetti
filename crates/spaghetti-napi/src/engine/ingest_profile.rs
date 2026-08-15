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
                "usage" => skip.usage = true,
                "extras" => skip.extras = true,
                "all-writes" => {
                    skip.facts = true;
                    skip.messages = true;
                    skip.runtime = true;
                    skip.delegation = true;
                    skip.artifact = true;
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
        assert!(skip.artifact && skip.usage && skip.extras);
        assert!(!skip.checkpoints && !skip.finalize);
        assert!(skip.relaxes_sqlite_constraints());
    }
}
