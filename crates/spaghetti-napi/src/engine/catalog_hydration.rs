//! Engine-owned RFC 012B selected-session hydration scheduling.
//!
//! Query workers prepare immutable commands from one exact retained snapshot.
//! This runtime then admits them to one bounded lane, preserves engine-lifetime
//! request replay/coalescing, and owns cancellation after an Accepted receipt.
//! Native locator authority never enters the portable command or receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{unbounded, Receiver, Sender};
use serde::Serialize;

use crate::catalog_contract::hydration::{
    CatalogHydrationActiveSchedule, CatalogHydrationActiveScheduleBinding,
    CatalogHydrationExecutionAuthorization, CatalogHydrationFailure,
    CatalogHydrationFailureDisposition, CatalogHydrationSchedulingOutcome,
    CatalogSchedulingReceipt,
};
use crate::catalog_contract::{CatalogHydrationCoalescingKey, CatalogHydrationRequestKey};

use super::catalog_query::{CatalogHydrationPreparationRequest, PreparedCatalogHydration};
use super::{EngineError, QueryCancellationToken, ReconcileOutcome, SpaghettiEngineCore};

const MAX_RETAINED_HYDRATION_REQUESTS: usize = 4_096;
const MAX_ACTIVE_HYDRATIONS: usize = 64;
const SOURCE_BUSY_RETRY_MILLIS: u32 = 250;
const SOURCE_CHANGED_RETRY_MILLIS: u32 = 100;
const SOURCE_UNAVAILABLE_RETRY_MILLIS: u32 = 1_000;
const STORAGE_RETRY_MILLIS: u32 = 5_000;
const INCOMPLETE_RETRY_MILLIS: u32 = 50;

#[derive(Default)]
pub(super) struct CatalogHydrationRuntime {
    scheduler: Option<CatalogHydrationScheduler>,
}

#[derive(Clone, Serialize)]
pub(crate) struct CatalogHydrationSchedulingResult {
    pub(crate) command: crate::catalog_contract::hydration::CatalogHydrationCommand,
    pub(crate) receipt: CatalogSchedulingReceipt,
    pub(crate) active_schedule: Option<CatalogHydrationActiveScheduleBinding>,
}

struct CatalogHydrationScheduler {
    commands: Sender<SchedulerCommand>,
    cancellation: QueryCancellationToken,
    alive: Arc<AtomicBool>,
    state: Arc<Mutex<CatalogHydrationSchedulerState>>,
    join: Option<JoinHandle<()>>,
}

enum SchedulerCommand {
    Execute(Box<HydrationJob>),
    Shutdown,
}

struct HydrationJob {
    coalescing_key: CatalogHydrationCoalescingKey,
    snapshot_complete_commit: u64,
    authorization: CatalogHydrationExecutionAuthorization,
    cancellation: QueryCancellationToken,
}

#[derive(Clone)]
struct ActiveHydration {
    command: crate::catalog_contract::hydration::CatalogHydrationCommand,
    accepted_receipt: CatalogSchedulingReceipt,
    request_keys: BTreeSet<CatalogHydrationRequestKey>,
    cancellation: QueryCancellationToken,
}

#[derive(Default)]
struct CatalogHydrationSchedulerState {
    commands: BTreeMap<
        CatalogHydrationRequestKey,
        crate::catalog_contract::hydration::CatalogHydrationCommand,
    >,
    receipts: BTreeMap<CatalogHydrationRequestKey, CatalogSchedulingReceipt>,
    active: BTreeMap<CatalogHydrationCoalescingKey, ActiveHydration>,
    satisfied: BTreeSet<CatalogHydrationCoalescingKey>,
}

struct Admission {
    receipt: CatalogSchedulingReceipt,
    execute: bool,
    prior_before_acceptance: Option<CatalogSchedulingReceipt>,
    execution_cancellation: Option<QueryCancellationToken>,
    active_schedule: Option<CatalogHydrationActiveScheduleBinding>,
}

struct SchedulerReplay {
    result: CatalogHydrationSchedulingResult,
    retryable: bool,
}

impl CatalogHydrationSchedulerState {
    fn admit(
        &mut self,
        command: &crate::catalog_contract::hydration::CatalogHydrationCommand,
        emitted_at_commit: u64,
    ) -> Result<Admission, EngineError> {
        let prior = match self.commands.get(&command.request_key) {
            Some(existing) if existing != command => {
                return Err(EngineError::InvalidQuery(
                    "catalog hydration request key cannot retarget immutable work".to_string(),
                ));
            }
            Some(_) => {
                let current = self.receipts.get(&command.request_key).ok_or_else(|| {
                    EngineError::InvalidCommit(
                        "catalog hydration replay lost its scheduling receipt".to_string(),
                    )
                })?;
                if !is_retryable_rejection(&current.outcome) {
                    return Ok(Admission {
                        receipt: current.clone(),
                        execute: false,
                        prior_before_acceptance: None,
                        execution_cancellation: None,
                        active_schedule: None,
                    });
                }
                Some(current.clone())
            }
            None => {
                if self.commands.len() >= MAX_RETAINED_HYDRATION_REQUESTS {
                    return Err(EngineError::QueryQueueFull);
                }
                self.commands.insert(command.request_key, command.clone());
                None
            }
        };

        if self.satisfied.contains(&command.coalescing_key) {
            let receipt = issue_receipt(
                command,
                prior.as_ref(),
                None,
                emitted_at_commit,
                CatalogHydrationSchedulingOutcome::AlreadySatisfied,
            )?;
            self.receipts.insert(command.request_key, receipt.clone());
            return Ok(Admission {
                receipt,
                execute: false,
                prior_before_acceptance: None,
                execution_cancellation: None,
                active_schedule: None,
            });
        }

        if let Some(active) = self.active.get_mut(&command.coalescing_key) {
            let active_schedule =
                CatalogHydrationActiveSchedule::new(&active.command, &active.accepted_receipt)
                    .map_err(super::catalog_state::catalog_contract_error)?;
            let receipt = issue_receipt(
                command,
                prior.as_ref(),
                Some(active_schedule),
                emitted_at_commit,
                CatalogHydrationSchedulingOutcome::InProgress {
                    active_command_id: active.command.command_id,
                    active_receipt_id: active.accepted_receipt.receipt_id,
                },
            )?;
            active.request_keys.insert(command.request_key);
            self.receipts.insert(command.request_key, receipt.clone());
            let active_schedule = CatalogHydrationActiveScheduleBinding::new(
                &active.command,
                &active.accepted_receipt,
            )
            .map_err(super::catalog_state::catalog_contract_error)?;
            return Ok(Admission {
                receipt,
                execute: false,
                prior_before_acceptance: None,
                execution_cancellation: None,
                active_schedule: Some(active_schedule),
            });
        }

        if self.active.len() >= MAX_ACTIVE_HYDRATIONS {
            let receipt = issue_receipt(
                command,
                prior.as_ref(),
                None,
                emitted_at_commit,
                retryable_outcome("scheduler_busy", SOURCE_BUSY_RETRY_MILLIS)?,
            )?;
            self.receipts.insert(command.request_key, receipt.clone());
            return Ok(Admission {
                receipt,
                execute: false,
                prior_before_acceptance: None,
                execution_cancellation: None,
                active_schedule: None,
            });
        }

        let execution_cancellation = QueryCancellationToken::default();
        let accepted = issue_receipt(
            command,
            prior.as_ref(),
            None,
            emitted_at_commit,
            CatalogHydrationSchedulingOutcome::Accepted,
        )?;
        self.active.insert(
            command.coalescing_key,
            ActiveHydration {
                command: command.clone(),
                accepted_receipt: accepted.clone(),
                request_keys: BTreeSet::from([command.request_key]),
                cancellation: execution_cancellation.clone(),
            },
        );
        self.receipts.insert(command.request_key, accepted.clone());
        Ok(Admission {
            receipt: accepted,
            execute: true,
            prior_before_acceptance: prior,
            execution_cancellation: Some(execution_cancellation),
            active_schedule: None,
        })
    }

    fn reject_unpublished_admission(
        &mut self,
        command: &crate::catalog_contract::hydration::CatalogHydrationCommand,
        prior: Option<&CatalogSchedulingReceipt>,
        emitted_at_commit: u64,
        failure_code: &'static str,
    ) -> Result<CatalogSchedulingReceipt, EngineError> {
        self.active.remove(&command.coalescing_key);
        let receipt = issue_receipt(
            command,
            prior,
            None,
            emitted_at_commit,
            retryable_outcome(failure_code, SOURCE_UNAVAILABLE_RETRY_MILLIS)?,
        )?;
        self.receipts.insert(command.request_key, receipt.clone());
        Ok(receipt)
    }

    fn complete(
        &mut self,
        coalescing_key: CatalogHydrationCoalescingKey,
        emitted_at_commit: u64,
        outcome: CatalogHydrationSchedulingOutcome,
    ) -> Result<(), EngineError> {
        let active = self.active.remove(&coalescing_key).ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog hydration completion has no accepted active command".to_string(),
            )
        })?;
        let mut completed = Vec::with_capacity(active.request_keys.len());
        for request_key in active.request_keys {
            let command = self.commands.get(&request_key).ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog hydration completion lost an admitted command".to_string(),
                )
            })?;
            let prior = self.receipts.get(&request_key).ok_or_else(|| {
                EngineError::InvalidCommit(
                    "catalog hydration completion lost its prior receipt".to_string(),
                )
            })?;
            completed.push((
                request_key,
                issue_receipt(
                    command,
                    Some(prior),
                    None,
                    emitted_at_commit,
                    outcome.clone(),
                )?,
            ));
        }
        for (request_key, receipt) in completed {
            self.receipts.insert(request_key, receipt);
        }
        if matches!(outcome, CatalogHydrationSchedulingOutcome::AlreadySatisfied) {
            self.satisfied.insert(coalescing_key);
        }
        Ok(())
    }

    fn cancel_adapter(&self, adapter_id: &str) {
        for active in self.active.values() {
            if active.command.authorization.adapter_id == adapter_id {
                active.cancellation.cancel();
            }
        }
    }

    fn cancel_all(&self) {
        for active in self.active.values() {
            active.cancellation.cancel();
        }
    }
}

impl CatalogHydrationScheduler {
    fn start(engine: Weak<SpaghettiEngineCore>) -> Result<Self, EngineError> {
        let (commands_tx, commands_rx) = unbounded();
        let cancellation = QueryCancellationToken::default();
        let thread_cancellation = cancellation.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let thread_alive = Arc::clone(&alive);
        let state = Arc::new(Mutex::new(CatalogHydrationSchedulerState::default()));
        let thread_state = Arc::clone(&state);
        let join = thread::Builder::new()
            .name("spaghetti-catalog-hydration".to_string())
            .spawn(move || {
                catalog_hydration_worker(
                    engine,
                    commands_rx,
                    thread_cancellation,
                    thread_alive,
                    thread_state,
                );
            })
            .map_err(|error| EngineError::WorkerStart {
                worker: "catalog hydration",
                detail: error.to_string(),
            })?;
        Ok(Self {
            commands: commands_tx,
            cancellation,
            alive,
            state,
            join: Some(join),
        })
    }

    fn schedule(
        &self,
        prepared: PreparedCatalogHydration,
        emitted_at_commit: u64,
    ) -> Result<CatalogHydrationSchedulingResult, EngineError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(EngineError::WorkerUnavailable {
                worker: "catalog hydration",
            });
        }
        let (command, authorization) = prepared.into_parts();
        if authorization.portable() != &command.authorization {
            return Err(EngineError::InvalidCommit(
                "catalog hydration native authority differs from its immutable command".to_string(),
            ));
        }
        let mut state = lock_state(&self.state);
        let admission = state.admit(&command, emitted_at_commit)?;
        let mut receipt = admission.receipt;
        if admission.execute
            && self
                .commands
                .send(SchedulerCommand::Execute(Box::new(HydrationJob {
                    coalescing_key: command.coalescing_key,
                    snapshot_complete_commit: command.snapshot_id.complete_commit,
                    authorization,
                    cancellation: admission
                        .execution_cancellation
                        .expect("executed hydration admission owns cancellation"),
                })))
                .is_err()
        {
            self.alive.store(false, Ordering::Release);
            receipt = state.reject_unpublished_admission(
                &command,
                admission.prior_before_acceptance.as_ref(),
                emitted_at_commit,
                "scheduler_unavailable",
            )?;
        }
        Ok(CatalogHydrationSchedulingResult {
            command,
            receipt,
            active_schedule: admission.active_schedule,
        })
    }

    fn replay(
        &self,
        request: &CatalogHydrationPreparationRequest,
    ) -> Result<Option<SchedulerReplay>, EngineError> {
        let state = lock_state(&self.state);
        let Some(command) = state.commands.get(&request.request_key()) else {
            return Ok(None);
        };
        request.validate_replay_command(command)?;
        let receipt = state.receipts.get(&request.request_key()).ok_or_else(|| {
            EngineError::InvalidCommit(
                "catalog hydration replay lost its scheduling receipt".to_string(),
            )
        })?;
        if !self.alive.load(Ordering::Acquire) && outcome_requires_worker(&receipt.outcome) {
            return Err(EngineError::WorkerUnavailable {
                worker: "catalog hydration",
            });
        }
        let active_schedule = match &receipt.outcome {
            CatalogHydrationSchedulingOutcome::InProgress {
                active_command_id,
                active_receipt_id,
            } => {
                let active = state.active.get(&receipt.coalescing_key).ok_or_else(|| {
                    EngineError::InvalidCommit(
                        "catalog hydration replay lost its active coalescing context".to_string(),
                    )
                })?;
                if active.command.command_id != *active_command_id
                    || active.accepted_receipt.receipt_id != *active_receipt_id
                {
                    return Err(EngineError::InvalidCommit(
                        "catalog hydration replay active context differs from its receipt"
                            .to_string(),
                    ));
                }
                Some(
                    CatalogHydrationActiveScheduleBinding::new(
                        &active.command,
                        &active.accepted_receipt,
                    )
                    .map_err(super::catalog_state::catalog_contract_error)?,
                )
            }
            _ => None,
        };
        Ok(Some(SchedulerReplay {
            result: CatalogHydrationSchedulingResult {
                command: command.clone(),
                receipt: receipt.clone(),
                active_schedule,
            },
            retryable: is_retryable_rejection(&receipt.outcome),
        }))
    }

    fn cancel_adapter(&self, adapter_id: &str) {
        lock_state(&self.state).cancel_adapter(adapter_id);
    }

    fn shutdown(&mut self) -> Result<(), EngineError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        lock_state(&self.state).cancel_all();
        self.cancellation.cancel();
        let _ = self.commands.send(SchedulerCommand::Shutdown);
        join.join().map_err(|_| EngineError::WorkerPanic {
            worker: "catalog hydration",
        })
    }
}

impl Drop for CatalogHydrationScheduler {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn catalog_hydration_worker(
    engine: Weak<SpaghettiEngineCore>,
    commands: Receiver<SchedulerCommand>,
    cancellation: QueryCancellationToken,
    alive: Arc<AtomicBool>,
    state: Arc<Mutex<CatalogHydrationSchedulerState>>,
) {
    let _alive_guard = CatalogHydrationWorkerAliveGuard(Arc::clone(&alive));
    while let Ok(command) = commands.recv() {
        let SchedulerCommand::Execute(job) = command else {
            break;
        };
        let HydrationJob {
            coalescing_key,
            snapshot_complete_commit,
            authorization,
            cancellation: execution_cancellation,
        } = *job;
        if cancellation.is_cancelled() {
            break;
        }
        let result = if execution_cancellation.is_cancelled() {
            Err(EngineError::QueryCancelled)
        } else {
            engine
                .upgrade()
                .ok_or(EngineError::ShuttingDown)
                .and_then(|engine| {
                    engine.execute_catalog_hydration(authorization, execution_cancellation)
                })
        };
        let outcome = match classify_hydration_result(result) {
            Ok(outcome) => outcome,
            Err(_) => break,
        };
        let emitted_at_commit = engine
            .upgrade()
            .map(|engine| engine.latest_commit_seq())
            .unwrap_or(snapshot_complete_commit)
            .max(snapshot_complete_commit);
        if lock_state(&state)
            .complete(coalescing_key, emitted_at_commit, outcome)
            .is_err()
        {
            break;
        }
    }
}

struct CatalogHydrationWorkerAliveGuard(Arc<AtomicBool>);

impl Drop for CatalogHydrationWorkerAliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn classify_hydration_result(
    result: Result<ReconcileOutcome, EngineError>,
) -> Result<CatalogHydrationSchedulingOutcome, EngineError> {
    match result {
        Ok(outcome) if outcome.objects_removed > 0 => terminal_outcome("source_unavailable"),
        Ok(outcome) if outcome.records_quarantined > 0 => terminal_outcome("records_quarantined"),
        Ok(outcome) if outcome.dependency_access_denials > 0 => {
            terminal_outcome("dependency_denied")
        }
        Ok(outcome) if outcome.streams_unavailable > 0 => {
            retryable_outcome("source_unavailable", SOURCE_UNAVAILABLE_RETRY_MILLIS)
        }
        Ok(outcome)
            if outcome.retries_required > 0
                || outcome.incomplete_tail_retries > 0
                || outcome.backlog_remaining > 0 =>
        {
            retryable_outcome("hydration_incomplete", INCOMPLETE_RETRY_MILLIS)
        }
        Ok(_) => Ok(CatalogHydrationSchedulingOutcome::AlreadySatisfied),
        Err(EngineError::ObservationBusy) => {
            retryable_outcome("source_busy", SOURCE_BUSY_RETRY_MILLIS)
        }
        Err(EngineError::StaleSourceCursor { .. }) => {
            retryable_outcome("source_changed", SOURCE_CHANGED_RETRY_MILLIS)
        }
        Err(EngineError::WorkerUnavailable { .. }) => {
            retryable_outcome("source_unavailable", SOURCE_UNAVAILABLE_RETRY_MILLIS)
        }
        Err(EngineError::Sqlite { .. } | EngineError::StorageCodec { .. }) => {
            retryable_outcome("engine_storage", STORAGE_RETRY_MILLIS)
        }
        Err(EngineError::InsufficientDiskSpace { .. }) => {
            retryable_outcome("insufficient_storage", STORAGE_RETRY_MILLIS)
        }
        Err(EngineError::QueryCancelled) => {
            retryable_outcome("source_unavailable", SOURCE_UNAVAILABLE_RETRY_MILLIS)
        }
        Err(EngineError::ShuttingDown) => {
            retryable_outcome("engine_stopping", SOURCE_UNAVAILABLE_RETRY_MILLIS)
        }
        Err(EngineError::Observation {
            operation: "hydrate selected catalog object",
            ..
        }) => terminal_outcome("stale_locator"),
        Err(EngineError::Observation { .. }) => {
            retryable_outcome("source_io", SOURCE_UNAVAILABLE_RETRY_MILLIS)
        }
        Err(EngineError::InvalidQuery(_) | EngineError::InvalidCommit(_)) => {
            terminal_outcome("stale_authority")
        }
        Err(EngineError::InvalidConfig(_)) => terminal_outcome("source_configuration"),
        Err(_) => terminal_outcome("hydration_failed"),
    }
}

fn retryable_outcome(
    code: &'static str,
    delay_millis: u32,
) -> Result<CatalogHydrationSchedulingOutcome, EngineError> {
    Ok(CatalogHydrationSchedulingOutcome::Rejected {
        failure: CatalogHydrationFailure::retryable(code, delay_millis)
            .map_err(super::catalog_state::catalog_contract_error)?,
    })
}

fn terminal_outcome(code: &'static str) -> Result<CatalogHydrationSchedulingOutcome, EngineError> {
    Ok(CatalogHydrationSchedulingOutcome::Rejected {
        failure: CatalogHydrationFailure::terminal(code)
            .map_err(super::catalog_state::catalog_contract_error)?,
    })
}

fn issue_receipt(
    command: &crate::catalog_contract::hydration::CatalogHydrationCommand,
    prior: Option<&CatalogSchedulingReceipt>,
    active: Option<CatalogHydrationActiveSchedule<'_>>,
    emitted_at_commit: u64,
    outcome: CatalogHydrationSchedulingOutcome,
) -> Result<CatalogSchedulingReceipt, EngineError> {
    CatalogSchedulingReceipt::issue(command, prior, active, emitted_at_commit, outcome)
        .map_err(super::catalog_state::catalog_contract_error)
}

fn is_retryable_rejection(outcome: &CatalogHydrationSchedulingOutcome) -> bool {
    matches!(
        outcome,
        CatalogHydrationSchedulingOutcome::Rejected {
            failure: CatalogHydrationFailure {
                disposition: CatalogHydrationFailureDisposition::Retryable,
                ..
            }
        }
    )
}

fn outcome_requires_worker(outcome: &CatalogHydrationSchedulingOutcome) -> bool {
    matches!(
        outcome,
        CatalogHydrationSchedulingOutcome::Accepted
            | CatalogHydrationSchedulingOutcome::InProgress { .. }
    )
}

fn lock_state(
    state: &Mutex<CatalogHydrationSchedulerState>,
) -> std::sync::MutexGuard<'_, CatalogHydrationSchedulerState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SpaghettiEngineCore {
    pub(crate) fn schedule_catalog_hydration(
        self: &Arc<Self>,
        request: CatalogHydrationPreparationRequest,
        cancellation: QueryCancellationToken,
    ) -> Result<CatalogHydrationSchedulingResult, EngineError> {
        let replay = self
            .lock_catalog_hydration()
            .scheduler
            .as_ref()
            .map(|scheduler| scheduler.replay(&request))
            .transpose()?
            .flatten();
        if let Some(replay) = &replay {
            if !replay.retryable {
                return Ok(replay.result.clone());
            }
        }
        let prepared = match self
            .query_client()?
            .prepare_catalog_hydration(request, cancellation)
        {
            Ok(prepared) => prepared,
            Err(EngineError::InvalidQuery(_)) if replay.is_some() => {
                return Ok(replay.expect("retryable replay was present").result);
            }
            Err(error) => return Err(error),
        };
        let emitted_at_commit = self
            .latest_commit_seq()
            .max(prepared.command().snapshot_id.complete_commit);
        let mut runtime = self.lock_catalog_hydration();
        if runtime.scheduler.is_none() {
            runtime.scheduler = Some(CatalogHydrationScheduler::start(Arc::downgrade(self))?);
        }
        runtime
            .scheduler
            .as_ref()
            .expect("catalog hydration scheduler was installed")
            .schedule(prepared, emitted_at_commit)
    }

    fn execute_catalog_hydration(
        &self,
        authorization: CatalogHydrationExecutionAuthorization,
        cancellation: QueryCancellationToken,
    ) -> Result<ReconcileOutcome, EngineError> {
        let adapter_id = authorization.portable().adapter_id.clone();
        let supervisors = self.lock_supervisors();
        let client = supervisors
            .iter()
            .find(|supervisor| supervisor.adapter_id() == adapter_id)
            .ok_or(EngineError::WorkerUnavailable {
                worker: "observation supervisor",
            })?
            .client();
        drop(supervisors);
        client.hydrate_selected_catalog_object(authorization, cancellation)
    }

    pub(super) fn clear_catalog_hydration(&self) -> Result<(), EngineError> {
        let mut scheduler = self.lock_catalog_hydration().scheduler.take();
        match scheduler.as_mut() {
            Some(scheduler) => scheduler.shutdown(),
            None => Ok(()),
        }
    }

    pub(super) fn cancel_catalog_hydration_for_adapter(&self, adapter_id: &str) {
        if let Some(scheduler) = self.lock_catalog_hydration().scheduler.as_ref() {
            scheduler.cancel_adapter(adapter_id);
        }
    }

    fn lock_catalog_hydration(&self) -> std::sync::MutexGuard<'_, CatalogHydrationRuntime> {
        self.catalog_hydration
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_contract::hydration::tests::scheduler_fixture_command;

    fn emitted(command: &crate::catalog_contract::hydration::CatalogHydrationCommand) -> u64 {
        command.snapshot_id.complete_commit.saturating_add(1)
    }

    #[test]
    fn scheduler_state_replays_rejects_retargeting_and_coalesces_exact_work() {
        let first = scheduler_fixture_command(b"scheduler-first", "selected-session");
        let coalesced = scheduler_fixture_command(b"scheduler-second", "selected-session");
        assert_ne!(first.request_key, coalesced.request_key);
        assert_eq!(first.coalescing_key, coalesced.coalescing_key);
        let mut state = CatalogHydrationSchedulerState::default();

        let accepted = state.admit(&first, emitted(&first)).unwrap();
        assert!(accepted.execute);
        assert!(accepted.active_schedule.is_none());
        assert!(matches!(
            accepted.receipt.outcome,
            CatalogHydrationSchedulingOutcome::Accepted
        ));
        let replay = state.admit(&first, emitted(&first) + 1).unwrap();
        assert!(!replay.execute);
        assert_eq!(replay.receipt, accepted.receipt);

        let follower = state.admit(&coalesced, emitted(&coalesced) + 1).unwrap();
        assert!(!follower.execute);
        let active_schedule = follower.active_schedule.as_ref().unwrap();
        assert_eq!(active_schedule.command, first.binding());
        assert_eq!(active_schedule.receipt, accepted.receipt);
        assert!(matches!(
            follower.receipt.outcome,
            CatalogHydrationSchedulingOutcome::InProgress {
                active_command_id,
                active_receipt_id,
            } if active_command_id == first.command_id
                && active_receipt_id == accepted.receipt.receipt_id
        ));

        let retargeted = scheduler_fixture_command(b"scheduler-first", "another-selected-session");
        assert!(matches!(
            state.admit(&retargeted, emitted(&retargeted)),
            Err(EngineError::InvalidQuery(_))
        ));

        state
            .complete(
                first.coalescing_key,
                emitted(&first) + 2,
                CatalogHydrationSchedulingOutcome::AlreadySatisfied,
            )
            .unwrap();
        for command in [&first, &coalesced] {
            let terminal = state.admit(command, emitted(command) + 3).unwrap();
            assert!(!terminal.execute);
            assert!(matches!(
                terminal.receipt.outcome,
                CatalogHydrationSchedulingOutcome::AlreadySatisfied
            ));
        }
        let later = scheduler_fixture_command(b"scheduler-third", "selected-session");
        let satisfied = state.admit(&later, emitted(&later) + 4).unwrap();
        assert!(!satisfied.execute);
        assert!(matches!(
            satisfied.receipt.outcome,
            CatalogHydrationSchedulingOutcome::AlreadySatisfied
        ));
    }

    #[test]
    fn retryable_completion_requires_a_new_attempt_while_terminal_is_sticky() {
        let retry = scheduler_fixture_command(b"scheduler-retry", "selected-session");
        let terminal = scheduler_fixture_command(b"scheduler-terminal", "terminal-session");
        let mut state = CatalogHydrationSchedulerState::default();

        let first = state.admit(&retry, emitted(&retry)).unwrap();
        state
            .complete(
                retry.coalescing_key,
                emitted(&retry) + 1,
                retryable_outcome("source_busy", SOURCE_BUSY_RETRY_MILLIS).unwrap(),
            )
            .unwrap();
        let retryable_receipt = state.receipts.get(&retry.request_key).unwrap().clone();
        let retried = state.admit(&retry, emitted(&retry) + 2).unwrap();
        assert!(retried.execute);
        assert_eq!(retried.receipt.attempt, 2);
        assert_eq!(
            retried.receipt.prior_receipt_id,
            Some(retryable_receipt.receipt_id)
        );
        assert_ne!(retried.receipt.receipt_id, first.receipt.receipt_id);

        state.admit(&terminal, emitted(&terminal)).unwrap();
        state
            .complete(
                terminal.coalescing_key,
                emitted(&terminal) + 1,
                terminal_outcome("stale_locator").unwrap(),
            )
            .unwrap();
        let replay = state.admit(&terminal, emitted(&terminal) + 2).unwrap();
        assert!(!replay.execute);
        assert_eq!(replay.receipt.attempt, 1);
        assert!(matches!(
            replay.receipt.outcome,
            CatalogHydrationSchedulingOutcome::Rejected {
                failure: CatalogHydrationFailure {
                    disposition: CatalogHydrationFailureDisposition::Terminal,
                    ..
                }
            }
        ));
    }

    #[test]
    fn scheduler_bounds_active_work_before_admitting_an_excess_command() {
        let mut state = CatalogHydrationSchedulerState::default();
        for index in 0..MAX_ACTIVE_HYDRATIONS {
            let command = scheduler_fixture_command(
                format!("scheduler-bound-{index}").as_bytes(),
                &format!("selected-{index}"),
            );
            let admission = state.admit(&command, emitted(&command)).unwrap();
            assert!(admission.execute);
        }
        let excess = scheduler_fixture_command(b"scheduler-bound-excess", "selected-excess");
        let rejected = state.admit(&excess, emitted(&excess)).unwrap();
        assert!(!rejected.execute);
        assert!(matches!(
            rejected.receipt.outcome,
            CatalogHydrationSchedulingOutcome::Rejected {
                failure: CatalogHydrationFailure {
                    disposition: CatalogHydrationFailureDisposition::Retryable,
                    ref code,
                    ..
                }
            } if code == "scheduler_busy"
        ));
        assert_eq!(state.active.len(), MAX_ACTIVE_HYDRATIONS);
    }

    #[test]
    fn adapter_cancellation_preserves_retained_receipts_and_only_cancels_matching_work() {
        let command = scheduler_fixture_command(b"scheduler-cancel", "selected-session");
        let mut state = CatalogHydrationSchedulerState::default();
        let admission = state.admit(&command, emitted(&command)).unwrap();
        let execution_cancellation = admission.execution_cancellation.unwrap();

        state.cancel_adapter("another-adapter");
        assert!(!execution_cancellation.is_cancelled());
        state.cancel_adapter(&command.authorization.adapter_id);
        assert!(execution_cancellation.is_cancelled());
        assert_eq!(state.commands.get(&command.request_key), Some(&command));
        assert_eq!(
            state.receipts.get(&command.request_key),
            Some(&admission.receipt)
        );
    }

    #[test]
    fn dead_worker_detection_covers_only_unfinished_receipts() {
        let command = scheduler_fixture_command(b"scheduler-active-command", "selected-session");
        let mut state = CatalogHydrationSchedulerState::default();
        let accepted = state.admit(&command, emitted(&command)).unwrap();
        assert!(outcome_requires_worker(
            &CatalogHydrationSchedulingOutcome::Accepted
        ));
        assert!(outcome_requires_worker(
            &CatalogHydrationSchedulingOutcome::InProgress {
                active_command_id: command.command_id,
                active_receipt_id: accepted.receipt.receipt_id,
            }
        ));
        assert!(!outcome_requires_worker(
            &CatalogHydrationSchedulingOutcome::AlreadySatisfied
        ));
        assert!(!outcome_requires_worker(
            &terminal_outcome("stale_locator").unwrap()
        ));

        let alive = Arc::new(AtomicBool::new(true));
        drop(CatalogHydrationWorkerAliveGuard(Arc::clone(&alive)));
        assert!(!alive.load(Ordering::Acquire));
    }

    #[test]
    fn execution_results_map_only_to_closed_path_free_machine_codes() {
        let busy = classify_hydration_result(Err(EngineError::ObservationBusy)).unwrap();
        let stale = classify_hydration_result(Err(EngineError::Observation {
            operation: "hydrate selected catalog object",
            detail: "/Users/alice/private/session.jsonl".to_string(),
        }))
        .unwrap();
        let incomplete = classify_hydration_result(Ok(ReconcileOutcome {
            backlog_remaining: 1,
            ..ReconcileOutcome::default()
        }))
        .unwrap();
        let encoded = serde_json::to_string(&(busy, stale, incomplete)).unwrap();
        assert!(!encoded.contains("/Users/"));
        assert!(!encoded.contains("alice"));
        assert!(encoded.contains("source_busy"));
        assert!(encoded.contains("stale_locator"));
        assert!(encoded.contains("hydration_incomplete"));
    }
}
