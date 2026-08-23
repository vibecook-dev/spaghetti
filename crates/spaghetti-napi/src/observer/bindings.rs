//! The N-API surface: one class, five methods.

use std::sync::Arc;
use std::time::Duration;

use crate::adapter::{AdapterId, AgentAdapter};

use napi::bindgen_prelude::{AsyncTask, Env, Error, Result, Status, Task};
use napi_derive::napi;

use super::{ObserveSessionRequest, ObserverEvent, ObserverHandle};

/// What the observer is currently doing, for a consumer's health surface.
///
/// This one crosses N-API as an object rather than through `ts-rs`, because
/// napi-rs already generates its TypeScript from the same declaration.
#[napi_derive::napi(object)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserverStatus {
    pub scope_epoch: i64,
    /// Highest sequence admitted so far. Not comparable across attachments and
    /// never comparable to a durable commit sequence.
    pub offered_through_sequence: i64,
    pub queued_semantic: u32,
    pub queued_control: u32,
    pub retained_bytes: u32,
    /// False between continuity loss and the completion of the replacement
    /// epoch. Ordinary semantic delivery is suspended while it is false.
    pub epoch_valid: bool,
    pub closed: bool,
}

/// Ceiling on one `poll` batch, so a caller cannot ask for an unbounded
/// serialization in one call.
const MAX_BATCH: u32 = 4_096;
const DEFAULT_BATCH: u32 = 256;

/// Store-free observer over one native session tree.
///
/// Create it with `observeSession(request)`. Every method is safe to call after
/// `close()`; the observer simply reports itself closed and stops delivering.
#[napi]
pub struct SpaghettiSessionObserver {
    handle: Arc<ObserverHandle>,
}

#[napi]
impl SpaghettiSessionObserver {
    #[napi(constructor, ts_args_type = "_notConstructible: never")]
    pub fn unsupported_constructor() -> Result<Self> {
        Err(Error::new(
            Status::InvalidArg,
            "SpaghettiSessionObserver cannot be constructed directly; use observeSession(request)",
        ))
    }

    /// Take up to `max` pending events as a JSON array of `ObserverEvent`.
    ///
    /// Returns immediately, including with an empty array. Calling it also
    /// hints the owner thread to reconcile now, which is the low-latency path
    /// a lifecycle hook should use.
    #[napi(js_name = "poll")]
    pub fn poll(&self, max: Option<u32>) -> Result<String> {
        let events = self.handle.poll(batch_size(max));
        encode(&events)
    }

    /// Wait up to `timeoutMs` for at least one event, then take up to `max`.
    /// Resolves with an empty array on timeout.
    #[napi(js_name = "waitForEvents", ts_return_type = "Promise<string>")]
    pub fn wait_for_events(&self, timeout_ms: u32, max: Option<u32>) -> AsyncTask<WaitForEvents> {
        AsyncTask::new(WaitForEvents {
            handle: Arc::clone(&self.handle),
            timeout: Duration::from_millis(u64::from(timeout_ms.min(600_000))),
            max: batch_size(max),
        })
    }

    /// Current epoch, queue depth, and whether continuity still holds.
    #[napi(js_name = "status")]
    pub fn status(&self) -> ObserverStatus {
        self.handle.status()
    }

    /// Release the scope. Idempotent, and waits for every owned watch, read,
    /// decode, and delivery to stop before it resolves.
    #[napi(js_name = "close", ts_return_type = "Promise<void>")]
    pub fn close(&self) -> AsyncTask<CloseObserver> {
        AsyncTask::new(CloseObserver {
            handle: Arc::clone(&self.handle),
        })
    }
}

pub struct WaitForEvents {
    handle: Arc<ObserverHandle>,
    timeout: Duration,
    max: usize,
}

impl Task for WaitForEvents {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        let events = self.handle.wait_for_events(self.timeout, self.max);
        encode(&events)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct CloseObserver {
    handle: Arc<ObserverHandle>,
}

impl Task for CloseObserver {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        self.handle.close();
        Ok(())
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

/// Open a store-free observer over one native session tree.
///
/// Accepts the request as an object or as a JSON string. Attachment is
/// synchronous in its validation: an unusable root, an identity mismatch, or an
/// unsupported adapter fails here rather than as an error event later.
#[napi(
    js_name = "observeSession",
    ts_args_type = "request: string | Record<string, unknown>"
)]
pub fn observe_session(request: serde_json::Value) -> Result<SpaghettiSessionObserver> {
    let request: ObserveSessionRequest = match request {
        serde_json::Value::String(json) => serde_json::from_str(&json),
        other => serde_json::from_value(other),
    }
    .map_err(|error| {
        Error::new(
            Status::InvalidArg,
            format!("observeSession request is invalid: {error}"),
        )
    })?;
    let adapter = resolve_adapter(&request.adapter_id)?;
    let handle = ObserverHandle::open(&request, adapter)
        .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?;
    Ok(SpaghettiSessionObserver {
        handle: Arc::new(handle),
    })
}

/// The binding layer is the composition root: it is the only part of the
/// observer that knows which adapters are compiled in.
fn resolve_adapter(adapter_id: &str) -> Result<Arc<dyn AgentAdapter>> {
    let id = AdapterId::new(adapter_id)
        .map_err(|error| Error::new(Status::InvalidArg, error.to_string()))?;
    let catalog = crate::napi_engine::verified_builtin_support_catalog()
        .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))?;
    let registry = crate::napi_engine::verified_builtin_registry(Arc::new(catalog))
        .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))?;
    registry
        .get(&id)
        .cloned()
        .ok_or_else(|| Error::new(Status::InvalidArg, format!("unknown adapter {adapter_id}")))
}

fn batch_size(max: Option<u32>) -> usize {
    max.unwrap_or(DEFAULT_BATCH).clamp(1, MAX_BATCH) as usize
}

/// Events cross as one JSON string. A batch of transcript events is several
/// kilobytes of deeply nested data; building that as JS values costs one N-API
/// call per node, while V8's own parser handles the whole batch in native code.
fn encode(events: &[ObserverEvent]) -> Result<String> {
    serde_json::to_string(events)
        .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))
}
