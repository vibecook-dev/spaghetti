//! Store-free observation of one native session tree (RFC 012D).
//!
//! The observer opens no database, no migration, no query pool, and no
//! whole-adapter host. It reaches only objects a declared relation names,
//! decodes them through the same `decode_record` boundary durable ingestion
//! uses, reduces them with the same RFC 012C family laws, and delivers typed
//! events on one ordered stream with deterministic ids and scope epochs.
//!
//! ```text
//!  declared scope  ->  append/replace driver  ->  decode_record  ->  FactBatch
//!                                                                       |
//!                                            runtime_semantic_reducer <--+
//!                                                       |
//!                                     bounded queue + control lane
//!                                                       |
//!                                             ObserverEvent stream
//! ```

mod bindings;
mod event;
mod identity;
mod object;
mod queue;
mod request;
mod runtime;
mod scope;
mod state;

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub use bindings::{observe_session, ObserverStatus, SpaghettiSessionObserver};
use crossbeam_channel::Sender;
pub use event::{
    ClosedEvent, FamilyManifestEntry, ObjectCoverage, ObserverBarrier, ObserverErrorEvent,
    ObserverEvent, ObserverFamily, ObserverPhase, OverflowEvent, OverflowReason, ResetEvent,
    SemanticEvent, SemanticOperation, SourceErrorEvent, SourcePosition, UnknownEvidenceEvent,
};
pub use identity::{ActorAttribution, ActorRef, ObserverEventId};
pub use request::ObserveSessionRequest;

use queue::Delivery;
use runtime::{nudge, signal_close, wake_channel, ObserverRuntime, Wake};

/// Everything that can stop an observer from attaching.
#[derive(Debug, thiserror::Error)]
pub enum ObserverError {
    #[error("observe request is invalid: {0}")]
    InvalidRequest(String),
    #[error("root identity cannot be settled: {0}")]
    InvalidRootIdentity(String),
    #[error("adapter {0} does not support scoped observation")]
    UnsupportedAdapter(String),
    #[error("scoped observation is unavailable: {0}")]
    Unsupported(String),
    #[error("adapter contract error: {0}")]
    Adapter(String),
    #[error("source error: {0}")]
    Source(String),
}

/// A live attachment. Cloning the handle shares one scope; dropping every
/// clone cancels the owner thread, but only `close()` waits for it.
pub struct ObserverHandle {
    delivery: Arc<Delivery>,
    wake: Sender<Wake>,
    owner: Mutex<Option<JoinHandle<()>>>,
}

impl ObserverHandle {
    /// Attach synchronously, then hand the scope to its owner thread.
    ///
    /// The caller supplies the adapter, which is the only place a vendor is
    /// named. Identity, adapter support, and the declared scope program are all
    /// settled here, so a bad request fails the call rather than surfacing as
    /// an error event later.
    pub fn open(
        request: &ObserveSessionRequest,
        adapter: Arc<dyn crate::adapter::AgentAdapter>,
    ) -> Result<Self, ObserverError> {
        let resolved = request.resolve(adapter.manifest().id.as_str())?;
        let delivery = Arc::new(Delivery::new(resolved.queue));
        let runtime = ObserverRuntime::attach(resolved, adapter, Arc::clone(&delivery))?;
        let (sender, receiver) = wake_channel();
        let hint = sender.clone();
        let owner = std::thread::Builder::new()
            .name("spaghetti-observer".to_string())
            .spawn(move || runtime.run(receiver, hint))
            .map_err(|error| ObserverError::Unsupported(error.to_string()))?;
        Ok(Self {
            delivery,
            wake: sender,
            owner: Mutex::new(Some(owner)),
        })
    }

    /// Take up to `max` events without blocking, and hint the owner thread to
    /// run a pass so the next poll sees fresh work.
    pub fn poll(&self, max: usize) -> Vec<ObserverEvent> {
        nudge(&self.wake);
        self.delivery.drain(max)
    }

    /// Wait until at least one event is available or the timeout expires.
    pub fn wait_for_events(&self, timeout: Duration, max: usize) -> Vec<ObserverEvent> {
        nudge(&self.wake);
        self.delivery.wait_for_events(timeout, max)
    }

    pub fn status(&self) -> ObserverStatus {
        let status = self.delivery.status();
        ObserverStatus {
            scope_epoch: i64::try_from(status.epoch).unwrap_or(i64::MAX),
            offered_through_sequence: i64::try_from(status.offered_through_sequence)
                .unwrap_or(i64::MAX),
            queued_semantic: status.queued_semantic,
            queued_control: status.queued_control,
            retained_bytes: status.retained_bytes,
            epoch_valid: status.epoch_valid,
            closed: status.closed,
        }
    }

    /// Idempotent. Rejects new source work, cancels waiters, and waits for the
    /// owner thread to stop before returning.
    pub fn close(&self) {
        self.delivery.request_close();
        signal_close(&self.wake);
        let owner = self
            .owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(owner) = owner {
            let _unused = owner.join();
        }
    }
}

impl Drop for ObserverHandle {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests;
