//! The typed payload an RFC 012D semantic event carries.
//!
//! An observer event's `value` is `serde_json::to_value(&Fact)` — the
//! externally tagged form of the fact the reducer accepted. `Fact` itself
//! cannot cross to TypeScript: most of its variants are RFC 011 durable facts
//! that never reach an observer, and several carry raw native payloads that
//! deliberately have no wire type.
//!
//! `RuntimeSemanticValue` is the runtime subset, with the same variant names
//! and the same inner types, so its serialization is byte-identical to the
//! `Fact` serialization it mirrors. That equality is not a convention to
//! remember — [`tests`] asserts it for every family, which is what lets the
//! generated TypeScript describe the real wire shape instead of a hand-kept
//! parallel definition.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::facts::EffectiveStateRevisionFact;
use super::facts::{
    ActorAffiliationRevisionFact, ActorRunRevisionFact, ContentBlockRevisionFact, Fact,
    MessageRevisionFact, NativeRuntimeMarkerRevisionFact, PlanRevisionFact, TaskRevisionFact,
    ToolRevisionFact, UsageRevisionV2Fact, UserInputRequestRevisionFact,
};

/// One RFC 012C revision, in the shape it crosses the observer wire.
///
/// The two widest arms are boxed: an enum is as wide as its widest variant, and
/// a qualified usage snapshot or a typed question set dwarfs the rest. `Box` is
/// transparent to serde and to ts-rs, so neither the wire shape nor the
/// generated TypeScript moves — the test below and the committed bindings both
/// hold it to that.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum RuntimeSemanticValue {
    ActorRunRevision(ActorRunRevisionFact),
    ActorAffiliationRevision(ActorAffiliationRevisionFact),
    UserInputRequestRevision(Box<UserInputRequestRevisionFact>),
    MessageRevision(MessageRevisionFact),
    ContentBlockRevision(ContentBlockRevisionFact),
    NativeRuntimeMarkerRevision(NativeRuntimeMarkerRevisionFact),
    TaskRevision(TaskRevisionFact),
    PlanRevision(PlanRevisionFact),
    ToolRevision(ToolRevisionFact),
    EffectiveStateRevision(EffectiveStateRevisionFact),
    UsageRevisionV2(Box<UsageRevisionV2Fact>),
}

impl RuntimeSemanticValue {
    /// The runtime view of a fact, or `None` when the fact belongs to a
    /// durable-only family that no observer event carries.
    pub fn from_fact(fact: &Fact) -> Option<Self> {
        Some(match fact {
            Fact::ActorRunRevision(value) => Self::ActorRunRevision(value.clone()),
            Fact::ActorAffiliationRevision(value) => Self::ActorAffiliationRevision(value.clone()),
            Fact::UserInputRequestRevision(value) => {
                Self::UserInputRequestRevision(Box::new(value.clone()))
            }
            Fact::MessageRevision(value) => Self::MessageRevision(value.clone()),
            Fact::ContentBlockRevision(value) => Self::ContentBlockRevision(value.clone()),
            Fact::NativeRuntimeMarkerRevision(value) => {
                Self::NativeRuntimeMarkerRevision(value.clone())
            }
            Fact::TaskRevision(value) => Self::TaskRevision(value.clone()),
            Fact::PlanRevision(value) => Self::PlanRevision(value.clone()),
            Fact::ToolRevision(value) => Self::ToolRevision(value.clone()),
            Fact::EffectiveStateRevision(value) => Self::EffectiveStateRevision(value.clone()),
            Fact::UsageRevisionV2(value) => Self::UsageRevisionV2(Box::new(value.clone())),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests;
