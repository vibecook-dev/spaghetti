//! Strict N-API owner for the first store-free RFC 012D observer transport.
//!
//! The JavaScript boundary carries bounded JSON strings only. Native paths are
//! accepted solely by the already-validated configured attachment request and
//! never appear in errors or portable results. Application receipts remain in
//! this process and are acknowledged through an explicit method after the SDK
//! successfully applies the matching envelope.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::{
    AsyncBlock, AsyncBlockBuilder, Env, Error, Result, Status, Utf16String,
};
use napi_derive::napi;

use crate::scoped_observation::configured_attachment::{
    prepare_configured_scoped_observation_attachment, ConfiguredScopedObservationRuntimeOptions,
    ConfiguredScopedObservationSupervisorRunResult,
};
use crate::scoped_observation::{
    ScopedObservationApplicationReceipt, ScopedObservationAsyncHandle,
    ScopedObservationAsyncRuntime, ScopedObservationContextualPollResolution,
    ScopedObservationReadyResolution, ScopedObservationResyncResolution,
};
use crate::scoped_observation_transport::{
    parse_public_scoped_observation_request, MAX_PUBLIC_SCOPED_OBSERVATION_REQUEST_JSON_BYTES,
};

const PUBLIC_MAX_FACTS_PER_RECORD: usize = 4_096;
const PUBLIC_MAX_DIAGNOSTICS_PER_RECORD: usize = 256;

struct SpaghettiSessionObserverInner {
    runtime: tokio::sync::Mutex<ScopedObservationAsyncRuntime>,
    handle: ScopedObservationAsyncHandle,
    pending_receipt: Mutex<Option<ScopedObservationApplicationReceipt>>,
    supervisor:
        Mutex<Option<tokio::task::JoinHandle<ConfiguredScopedObservationSupervisorRunResult>>>,
    closed: AtomicBool,
}

impl SpaghettiSessionObserverInner {
    fn request_drop_close(&self) {
        let _ = self.handle.request_close();
        if let Some(supervisor) = self
            .supervisor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            supervisor.abort();
        }
    }
}

impl Drop for SpaghettiSessionObserverInner {
    fn drop(&mut self) {
        self.request_drop_close();
    }
}

/// Native owner for one store-free RFC 012D session attachment. Construct it
/// through `openScopedObservationJson`; direct construction is forbidden.
#[napi]
pub struct SpaghettiSessionObserver {
    inner: Arc<SpaghettiSessionObserverInner>,
}

#[napi]
impl SpaghettiSessionObserver {
    #[napi(constructor, ts_args_type = "_notConstructible: never")]
    pub fn unsupported_constructor() -> Result<Self> {
        Err(Error::new(
            Status::InvalidArg,
            "SpaghettiSessionObserver cannot be constructed directly; use openScopedObservationJson(requestJson)",
        ))
    }

    /// Return the attachment's immutable capability snapshot plus the exact
    /// portable parsing context. Neither value carries source-access authority.
    #[napi(js_name = "capabilitiesJson")]
    pub fn capabilities_json(&self) -> Result<String> {
        ensure_open(&self.inner)?;
        let value = self
            .inner
            .handle
            .capability_snapshot_wire_value()
            .map_err(|_| observer_operation_error())?;
        serde_json::to_string(&value).map_err(|_| observer_operation_error())
    }

    /// Yield one strict outer-union envelope. The matching process-local
    /// receipt remains pending until `acknowledgeApplied()` succeeds.
    #[napi(js_name = "nextEventJson", ts_return_type = "Promise<string | null>")]
    pub fn next_event_json(&self, env: Env) -> Result<AsyncBlock<Option<String>>> {
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            ensure_open(&inner)?;
            let mut runtime = inner.runtime.lock().await;
            if inner
                .pending_receipt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some()
            {
                return Err(application_pending_error());
            }
            let Some(yielded) = runtime
                .next_event()
                .await
                .map_err(|_| observer_operation_error())?
            else {
                return Ok(None);
            };
            let json = serde_json::to_string(&yielded.event_union_value())
                .map_err(|_| observer_operation_error())?;
            *inner
                .pending_receipt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(yielded.application_receipt().clone());
            Ok(Some(json))
        })
        .build(&env)
    }

    /// Advance the consumer-owned applied boundary for the last yielded
    /// envelope. No receipt bytes cross N-API or enter portable JSON.
    #[napi(js_name = "acknowledgeApplied", ts_return_type = "Promise<void>")]
    pub fn acknowledge_applied(&self, env: Env) -> Result<AsyncBlock<()>> {
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            ensure_open(&inner)?;
            let receipt = inner
                .pending_receipt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .ok_or_else(no_pending_application_error)?;
            let mut runtime = inner.runtime.lock().await;
            runtime
                .acknowledge_applied(&receipt)
                .map_err(|_| observer_operation_error())?;
            let mut pending = inner
                .pending_receipt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if pending.as_ref().is_some_and(|candidate| {
                candidate.observer_sequence() == receipt.observer_sequence()
            }) {
                *pending = None;
            }
            Ok(())
        })
        .build(&env)
    }

    /// Run one exact-scope pass and return its contextual strict watermark.
    #[napi(js_name = "pollJson", ts_return_type = "Promise<string>")]
    pub fn poll_json(&self, env: Env) -> Result<AsyncBlock<String>> {
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            ensure_open(&inner)?;
            let resolution = inner
                .handle
                .poll_contextual()
                .await
                .map_err(|_| observer_operation_error())?;
            let ScopedObservationContextualPollResolution::Ready(completed) = resolution else {
                return Err(observer_operation_error());
            };
            let context = completed
                .context_wire_value()
                .map_err(|_| observer_operation_error())?;
            let watermark = completed
                .watermark_wire_value()
                .map_err(|_| observer_operation_error())?;
            serde_json::to_string(&serde_json::json!({
                "context": context,
                "watermark": watermark,
            }))
            .map_err(|_| observer_operation_error())
        })
        .build(&env)
    }

    /// Await engine-level bootstrap admission. The completion envelope still
    /// belongs to `nextEventJson()` and must be applied independently.
    #[napi(js_name = "readyOffered", ts_return_type = "Promise<void>")]
    pub fn ready_offered(&self, env: Env) -> Result<AsyncBlock<()>> {
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            ensure_open(&inner)?;
            match inner
                .handle
                .ready()
                .await
                .map_err(|_| observer_operation_error())?
            {
                ScopedObservationReadyResolution::Ready(_) => Ok(()),
                ScopedObservationReadyResolution::Pending
                | ScopedObservationReadyResolution::Failed(_)
                | ScopedObservationReadyResolution::Cancelled => Err(observer_operation_error()),
            }
        })
        .build(&env)
    }

    /// Request a full-snapshot replacement and await its engine-offered
    /// barrier. Ordered delivery and application remain on the event drain.
    #[napi(js_name = "resyncOffered", ts_return_type = "Promise<void>")]
    pub fn resync_offered(&self, env: Env) -> Result<AsyncBlock<()>> {
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            ensure_open(&inner)?;
            match inner
                .handle
                .resync()
                .await
                .map_err(|_| observer_operation_error())?
            {
                ScopedObservationResyncResolution::Ready(_) => Ok(()),
                ScopedObservationResyncResolution::Failed(_)
                | ScopedObservationResyncResolution::Cancelled => Err(observer_operation_error()),
            }
        })
        .build(&env)
    }

    /// Idempotently cancel the attachment, await all owned work, and join the
    /// retained producer supervisor before resolving.
    #[napi(js_name = "close", ts_return_type = "Promise<void>")]
    pub fn close(&self, env: Env) -> Result<AsyncBlock<()>> {
        let inner = Arc::clone(&self.inner);
        AsyncBlockBuilder::new(async move {
            let supervisor = inner
                .supervisor
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            let close = inner.handle.close_contextual();
            match supervisor {
                Some(supervisor) => {
                    let (close_result, supervisor_result) = tokio::join!(close, supervisor);
                    close_result.map_err(|_| observer_operation_error())?;
                    let _supervisor_outcome =
                        supervisor_result.map_err(|_| observer_operation_error())?;
                }
                None => {
                    close.await.map_err(|_| observer_operation_error())?;
                }
            }
            inner.closed.store(true, Ordering::Release);
            *inner
                .pending_receipt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            Ok(())
        })
        .build(&env)
    }
}

/// Open one configured, exact-known-object RFC 012D observer. Current built-in
/// Candidate releases fail closed here; this function does not create a test
/// authorization or bypass promotion state.
#[napi(
    js_name = "openScopedObservationJson",
    ts_return_type = "Promise<SpaghettiSessionObserver>"
)]
pub fn open_scoped_observation_json(
    env: Env,
    request_json: Utf16String,
) -> Result<AsyncBlock<SpaghettiSessionObserver>> {
    let request_json = bounded_request_json(request_json)?;
    let request = parse_public_scoped_observation_request(&request_json)
        .map_err(|_| invalid_request_error())?;
    AsyncBlockBuilder::new(async move {
        let opened = tokio::task::spawn_blocking(move || {
            let support_catalog = crate::napi_engine::verified_builtin_support_catalog()
                .map(Arc::new)
                .map_err(|_| PublicObserverOpenFailure::Invalid)?;
            let registry = crate::napi_engine::verified_builtin_registry(support_catalog)
                .map_err(|_| PublicObserverOpenFailure::Invalid)?;
            let attachment = prepare_configured_scoped_observation_attachment(&registry, request)
                .map_err(|_| PublicObserverOpenFailure::Invalid)?
                .ok_or(PublicObserverOpenFailure::Unavailable)?;
            attachment
                .prepare_append_runtime(
                    PUBLIC_MAX_FACTS_PER_RECORD,
                    PUBLIC_MAX_DIAGNOSTICS_PER_RECORD,
                )
                .map_err(|_| PublicObserverOpenFailure::Invalid)?
                .open(ConfiguredScopedObservationRuntimeOptions::default())
                .map_err(|_| PublicObserverOpenFailure::Invalid)
        })
        .await
        .map_err(|_| observer_open_error())?
        .map_err(|failure| match failure {
            PublicObserverOpenFailure::Unavailable => observer_unavailable_error(),
            PublicObserverOpenFailure::Invalid => observer_open_error(),
        })?;
        let (runtime, handle, supervisor) = opened.into_parts();
        let supervisor = tokio::spawn(supervisor.run_until_stopped());
        Ok(SpaghettiSessionObserver {
            inner: Arc::new(SpaghettiSessionObserverInner {
                runtime: tokio::sync::Mutex::new(runtime),
                handle,
                pending_receipt: Mutex::new(None),
                supervisor: Mutex::new(Some(supervisor)),
                closed: AtomicBool::new(false),
            }),
        })
    })
    .build(&env)
}

fn bounded_request_json(json: Utf16String) -> Result<String> {
    bounded_request_utf16(&json).map_err(|_| invalid_request_error())
}

fn bounded_request_utf16(json: &[u16]) -> std::result::Result<String, ()> {
    if json.len() > MAX_PUBLIC_SCOPED_OBSERVATION_REQUEST_JSON_BYTES {
        return Err(());
    }
    let json = String::from_utf16(json).map_err(|_| ())?;
    if json.is_empty() || json.len() > MAX_PUBLIC_SCOPED_OBSERVATION_REQUEST_JSON_BYTES {
        return Err(());
    }
    Ok(json)
}

fn ensure_open(inner: &SpaghettiSessionObserverInner) -> Result<()> {
    if inner.closed.load(Ordering::Acquire) {
        Err(observer_closed_error())
    } else {
        Ok(())
    }
}

fn invalid_request_error() -> Error {
    Error::new(Status::InvalidArg, "invalid scoped observation request")
}

fn observer_unavailable_error() -> Error {
    Error::new(
        Status::GenericFailure,
        "scoped observation is unavailable for this artifact",
    )
}

fn observer_open_error() -> Error {
    Error::new(Status::GenericFailure, "could not open scoped observation")
}

fn observer_operation_error() -> Error {
    Error::new(
        Status::GenericFailure,
        "scoped observation operation failed",
    )
}

fn observer_closed_error() -> Error {
    Error::new(Status::GenericFailure, "scoped observation is closed")
}

fn application_pending_error() -> Error {
    Error::new(
        Status::GenericFailure,
        "scoped observation application acknowledgement is pending",
    )
}

fn no_pending_application_error() -> Error {
    Error::new(
        Status::InvalidArg,
        "scoped observation has no pending application acknowledgement",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicObserverOpenFailure {
    Unavailable,
    Invalid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_request_rejects_unpaired_and_oversized_utf16_without_echoing_input() {
        assert_eq!(bounded_request_utf16(&[0xd800]), Err(()));

        let oversized = vec![b'x' as u16; MAX_PUBLIC_SCOPED_OBSERVATION_REQUEST_JSON_BYTES + 1];
        assert_eq!(bounded_request_utf16(&oversized), Err(()));
    }
}
