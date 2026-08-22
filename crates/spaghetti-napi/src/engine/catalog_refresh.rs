//! Bounded all-source RFC 012B catalog refresh scheduling.
//!
//! Observation supervisors only report that committed native state changed.
//! One engine-owned worker coalesces those reports and rebuilds the complete
//! configured catalog plan. No adapter-specific worker can publish a partial
//! catalog or independently choose source authority.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TrySendError};

use super::catalog_build::{
    CatalogBuildIntent, CatalogBuildOutcome, CatalogConfiguredSource, CatalogPublicationKind,
};
use super::{EngineError, QueryCancellationToken, SpaghettiEngineCore};

const CATALOG_REFRESH_WAKE_CAPACITY: usize = 1;
const CATALOG_REFRESH_COALESCE_WINDOW: Duration = Duration::from_millis(50);
const CATALOG_REFRESH_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_AUTOMATIC_REFRESH_ATTEMPTS: usize = 3;

type RefreshOperation =
    Box<dyn Fn(&QueryCancellationToken) -> Result<(), EngineError> + Send + 'static>;

#[derive(Default)]
pub(super) struct ConfiguredCatalogRefreshRuntime {
    configured: Option<Vec<CatalogConfiguredSource>>,
    scheduler: Option<CatalogRefreshScheduler>,
    pending: bool,
}

struct CatalogRefreshScheduler {
    wake: Sender<()>,
    cancellation: QueryCancellationToken,
    alive: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl CatalogRefreshScheduler {
    fn start(
        engine: Weak<SpaghettiEngineCore>,
        configured: Vec<CatalogConfiguredSource>,
    ) -> Result<Self, EngineError> {
        Self::start_with_policy(
            move |cancellation| {
                let engine = engine.upgrade().ok_or(EngineError::ShuttingDown)?;
                let _pass = engine
                    .source_pass_pool
                    .as_ref()
                    .map(crate::source::SharedSourcePassPool::blocking_acquire);
                match engine.reconcile_configured_catalog(
                    configured.clone(),
                    CatalogBuildIntent::Refresh,
                    cancellation.clone(),
                )? {
                    CatalogBuildOutcome::Published {
                        kind: CatalogPublicationKind::Refresh,
                        ..
                    } => Ok(()),
                    CatalogBuildOutcome::AuthorizationUnavailable { .. }
                    | CatalogBuildOutcome::LastCompleteRetained
                    | CatalogBuildOutcome::Published {
                        kind: CatalogPublicationKind::Initial,
                        ..
                    } => Err(EngineError::InvalidCommit(
                        "configured catalog refresh lost its active promoted lineage".to_string(),
                    )),
                }
            },
            CATALOG_REFRESH_COALESCE_WINDOW,
            CATALOG_REFRESH_RETRY_DELAY,
            MAX_AUTOMATIC_REFRESH_ATTEMPTS,
        )
    }

    fn start_with_policy<F>(
        operation: F,
        coalesce_window: Duration,
        retry_delay: Duration,
        max_attempts: usize,
    ) -> Result<Self, EngineError>
    where
        F: Fn(&QueryCancellationToken) -> Result<(), EngineError> + Send + 'static,
    {
        if max_attempts == 0 {
            return Err(EngineError::InvalidConfig(
                "catalog refresh scheduler requires at least one attempt".to_string(),
            ));
        }
        let (wake_tx, wake_rx) = bounded(CATALOG_REFRESH_WAKE_CAPACITY);
        let cancellation = QueryCancellationToken::default();
        let thread_cancellation = cancellation.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let thread_alive = Arc::clone(&alive);
        let operation: RefreshOperation = Box::new(operation);
        let join = thread::Builder::new()
            .name("spaghetti-catalog-refresh".to_string())
            .spawn(move || {
                catalog_refresh_worker(
                    wake_rx,
                    thread_cancellation,
                    thread_alive,
                    operation,
                    coalesce_window,
                    retry_delay,
                    max_attempts,
                );
            })
            .map_err(|error| EngineError::WorkerStart {
                worker: "catalog refresh",
                detail: error.to_string(),
            })?;
        Ok(Self {
            wake: wake_tx,
            cancellation,
            alive,
            join: Some(join),
        })
    }

    fn request(&self) -> Result<(), EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable {
                worker: "catalog refresh",
            });
        }
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => Err(EngineError::WorkerUnavailable {
                worker: "catalog refresh",
            }),
        }
    }

    fn shutdown(&mut self) -> Result<(), EngineError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        self.cancellation.cancel();
        let _ = self.wake.try_send(());
        join.join().map_err(|_| EngineError::WorkerPanic {
            worker: "catalog refresh",
        })
    }
}

impl Drop for CatalogRefreshScheduler {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn catalog_refresh_worker(
    wake: Receiver<()>,
    cancellation: QueryCancellationToken,
    alive: Arc<AtomicBool>,
    operation: RefreshOperation,
    coalesce_window: Duration,
    retry_delay: Duration,
    max_attempts: usize,
) {
    let mut attempts: usize = 0;
    let mut retry = false;
    loop {
        let external_wake = if retry {
            match wake.recv_timeout(retry_delay) {
                Ok(()) => true,
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match wake.recv() {
                Ok(()) => true,
                Err(_) => break,
            }
        };
        if cancellation.is_cancelled() {
            break;
        }
        if external_wake {
            attempts = 0;
        }

        let deadline = Instant::now() + coalesce_window;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match wake.recv_timeout(remaining) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    alive.store(false, Ordering::Release);
                    return;
                }
            }
        }
        if cancellation.is_cancelled() {
            break;
        }

        match operation(&cancellation) {
            Ok(()) => {
                attempts = 0;
                retry = false;
            }
            Err(EngineError::QueryCancelled | EngineError::ShuttingDown)
                if cancellation.is_cancelled() =>
            {
                break;
            }
            Err(_) => {
                attempts = attempts.saturating_add(1);
                retry = attempts < max_attempts;
            }
        }
    }
    alive.store(false, Ordering::Release);
}

impl SpaghettiEngineCore {
    pub(super) fn stage_configured_catalog_refresh(
        &self,
        configured: Vec<CatalogConfiguredSource>,
    ) -> Result<(), EngineError> {
        if configured.is_empty() {
            return Err(EngineError::InvalidConfig(
                "configured catalog refresh requires at least one source".to_string(),
            ));
        }
        let mut runtime = self.lock_catalog_refresh();
        if runtime.configured.is_some() || runtime.scheduler.is_some() {
            return Err(EngineError::InvalidConfig(
                "configured catalog refresh is already installed".to_string(),
            ));
        }
        runtime.configured = Some(configured);
        runtime.pending = false;
        Ok(())
    }

    pub(super) fn activate_configured_catalog_refresh(self: &Arc<Self>) -> Result<(), EngineError> {
        let mut runtime = self.lock_catalog_refresh();
        if runtime.scheduler.is_some() {
            return Err(EngineError::InvalidConfig(
                "configured catalog refresh is already active".to_string(),
            ));
        }
        let configured = runtime.configured.clone().ok_or_else(|| {
            EngineError::InvalidConfig(
                "configured catalog refresh was not staged before activation".to_string(),
            )
        })?;
        let scheduler = CatalogRefreshScheduler::start(Arc::downgrade(self), configured)?;
        if runtime.pending {
            scheduler.request()?;
        }
        runtime.scheduler = Some(scheduler);
        runtime.pending = false;
        Ok(())
    }

    pub(crate) fn request_configured_catalog_refresh(&self) -> Result<(), EngineError> {
        let mut runtime = self.lock_catalog_refresh();
        if runtime.configured.is_none() {
            return Ok(());
        }
        let Some(scheduler) = runtime.scheduler.as_ref() else {
            runtime.pending = true;
            return Ok(());
        };
        if let Err(error) = scheduler.request() {
            runtime.pending = true;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn clear_configured_catalog_refresh(&self) -> Result<(), EngineError> {
        let mut scheduler = {
            let mut runtime = self.lock_catalog_refresh();
            runtime.configured = None;
            runtime.pending = false;
            runtime.scheduler.take()
        };
        match scheduler.as_mut() {
            Some(scheduler) => scheduler.shutdown(),
            None => Ok(()),
        }
    }

    pub(super) fn clear_catalog_refresh_for_adapter(
        &self,
        adapter_id: &str,
    ) -> Result<(), EngineError> {
        let configured =
            self.lock_catalog_refresh()
                .configured
                .as_ref()
                .is_some_and(|configured| {
                    configured
                        .iter()
                        .any(|source| source.adapter_id() == adapter_id)
                });
        if configured {
            self.clear_configured_catalog_refresh()?;
        }
        Ok(())
    }

    fn lock_catalog_refresh(&self) -> std::sync::MutexGuard<'_, ConfiguredCatalogRefreshRuntime> {
        self.catalog_refresh
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub(super) fn configured_catalog_refresh_is_active(&self) -> bool {
        self.lock_catalog_refresh().scheduler.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crossbeam_channel::{bounded, unbounded};

    use super::*;

    #[test]
    fn active_refresh_coalesces_bursts_without_losing_a_late_change() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (called_tx, called_rx) = unbounded();
        let (release_tx, release_rx) = bounded(1);
        let worker_calls = Arc::clone(&calls);
        let mut scheduler = CatalogRefreshScheduler::start_with_policy(
            move |_| {
                let call = worker_calls.fetch_add(1, Ordering::SeqCst) + 1;
                called_tx.send(call).unwrap();
                if call == 1 {
                    release_rx.recv().unwrap();
                }
                Ok(())
            },
            Duration::from_millis(2),
            Duration::from_millis(5),
            3,
        )
        .unwrap();

        for _ in 0..16 {
            scheduler.request().unwrap();
        }
        assert_eq!(called_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        for _ in 0..16 {
            scheduler.request().unwrap();
        }
        release_tx.send(()).unwrap();
        assert_eq!(called_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
        assert!(called_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        scheduler.shutdown().unwrap();
    }

    #[test]
    fn refresh_failures_retry_a_bounded_number_of_times_per_signal() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (called_tx, called_rx) = unbounded();
        let worker_calls = Arc::clone(&calls);
        let mut scheduler = CatalogRefreshScheduler::start_with_policy(
            move |_| {
                let call = worker_calls.fetch_add(1, Ordering::SeqCst) + 1;
                called_tx.send(call).unwrap();
                Err(EngineError::Observation {
                    operation: "test catalog refresh",
                    detail: "retryable".to_string(),
                })
            },
            Duration::from_millis(1),
            Duration::from_millis(5),
            3,
        )
        .unwrap();

        scheduler.request().unwrap();
        for expected in 1..=3 {
            assert_eq!(
                called_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                expected
            );
        }
        assert!(called_rx.recv_timeout(Duration::from_millis(50)).is_err());

        scheduler.request().unwrap();
        for expected in 4..=6 {
            assert_eq!(
                called_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                expected
            );
        }
        assert!(called_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        scheduler.shutdown().unwrap();
    }
}
