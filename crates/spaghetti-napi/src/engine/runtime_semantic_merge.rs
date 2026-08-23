//! Crate-private RFC 012C runtime family fixtures.
//!
//! What lived here was a durable/scoped overlay merge that no caller ever
//! reached: the engine method wrapping it and the tests below it were its only
//! callers. The observer owns live delivery now, so the merge is gone and this
//! module keeps only what the observer's family tests still read — the typed
//! contribution shape and the committed RFC 012C fixture builder.

#[cfg(test)]
use crate::adapter::{Fact, FactSemanticRevision};

/// One current durable revision on the closed RFC 012C semantic boundary.
/// Only the observer's family tests read it now, so it does not exist in a
/// production build.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DurableRuntimeContribution {
    pub semantic: FactSemanticRevision,
    pub revision: Fact,
}

#[cfg(test)]
pub(crate) mod tests;
