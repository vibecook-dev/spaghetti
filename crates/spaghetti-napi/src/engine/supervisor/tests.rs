use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Condvar, Mutex};

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

use crate::adapter::{
    AdapterError, AdapterErrorClass, AdapterId, AdapterManifest, AdapterObjectContext,
    ConsistencyPolicy, DecodeContext, DecodeDisposition, DecoderId, DeletionPolicy,
    DiscoveryContext, DriverSpec, EntityScope, FactBatch, ObjectSelector, RawRetentionPolicy,
    SourceInstance, SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor, SourceRoot,
    StreamAuthority, StreamId, StreamSpec,
};
use crate::claude::ClaudeCodeAdapter;
use crate::engine::EngineOptions;
use crate::source::{
    platform_path_key, AppendDelimitedConfig, IngestPriority, SourceCursor, SourceRecord,
};

use super::*;

#[test]
fn dirty_reasons_map_to_bounded_global_priority_classes() {
    for reason in [DirtyReason::NativeEvent, DirtyReason::PollDetectedChange] {
        assert_eq!(
            priority_for_dirty_reason(reason),
            IngestPriority::Interactive
        );
    }
    assert_eq!(
        priority_for_dirty_reason(DirtyReason::Recovery),
        IngestPriority::Backfill
    );
    for reason in [
        DirtyReason::WatcherOverflow,
        DirtyReason::InternalQueueOverflow,
        DirtyReason::BackendError,
        DirtyReason::CursorInvalid,
        DirtyReason::IdentityChanged,
        DirtyReason::RootMoved,
        DirtyReason::ManualRepair,
    ] {
        assert_eq!(
            priority_for_dirty_reason(reason),
            IngestPriority::ForegroundRepair
        );
    }
}

fn unavailable_watcher(
    _engine: Weak<SpaghettiEngineCore>,
    _adapter_id: String,
    _topology: Arc<WatchTopology>,
    _wake: Sender<()>,
) -> Result<RecommendedWatcher, EngineError> {
    Err(EngineError::WorkerStart {
        worker: "native filesystem watcher",
        detail: "injected unavailable backend".to_string(),
    })
}

fn silent_watcher(
    _engine: Weak<SpaghettiEngineCore>,
    _adapter_id: String,
    _topology: Arc<WatchTopology>,
    _wake: Sender<()>,
) -> Result<RecommendedWatcher, EngineError> {
    notify::recommended_watcher(|_| {}).map_err(|error| EngineError::WorkerStart {
        worker: "test observation watcher",
        detail: error.to_string(),
    })
}

#[test]
fn prepared_supervisor_installs_watcher_without_starting_history_scan() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("first.jsonl"), b"{}\n").unwrap();
    let database = temp.path().join("prepared.db");
    let engine = open_engine(database.clone());

    let prepared = ObservationSupervisor::prepare_with_watcher_factory(
        Arc::clone(&engine),
        IgnoredAppendAdapter::new(),
        ObservationSupervisorOptions::new(vec![root.clone()]),
        silent_watcher,
        QueryCancellationToken::default(),
    )
    .unwrap();
    assert_eq!(prepared.inner().adapter_id, "ignored-append");
    assert_eq!(prepared.inner().watched_instances, 1);
    assert_eq!(prepared.inner().watch_roots, 1);
    assert!(prepared
        .inner()
        .watcher_available
        .load(AtomicOrdering::Acquire));

    let connection = Connection::open(&database).unwrap();
    let source_count: u64 = connection
        .query_row("SELECT COUNT(*) FROM source_instances", [], |row| {
            row.get(0)
        })
        .unwrap();
    let object_count: u64 = connection
        .query_row("SELECT COUNT(*) FROM source_objects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_count, 0, "preparation must not register by itself");
    assert_eq!(object_count, 0, "preparation must not scan source content");
    drop(connection);

    std::fs::write(root.join("second.jsonl"), b"{}\n").unwrap();
    let mut supervisor = prepared.start().unwrap();
    let connection = Connection::open(&database).unwrap();
    let source_count: u64 = connection
        .query_row("SELECT COUNT(*) FROM source_instances", [], |row| {
            row.get(0)
        })
        .unwrap();
    let object_count: u64 = connection
        .query_row("SELECT COUNT(*) FROM source_objects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_count, 1);
    assert_eq!(object_count, 2);
    drop(connection);

    supervisor.shutdown().unwrap();
    engine.shutdown().unwrap();
}

#[test]
fn cancelled_prepared_supervisor_exits_without_scanning() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("first.jsonl"), b"{}\n").unwrap();
    let database = temp.path().join("cancelled-prepared.db");
    let engine = open_engine(database.clone());
    let cancellation = QueryCancellationToken::default();
    let prepared = ObservationSupervisor::prepare_with_watcher_factory(
        Arc::clone(&engine),
        IgnoredAppendAdapter::new(),
        ObservationSupervisorOptions::new(vec![root]),
        silent_watcher,
        cancellation.clone(),
    )
    .unwrap();

    cancellation.cancel();
    assert!(matches!(prepared.start(), Err(EngineError::QueryCancelled)));
    let connection = Connection::open(database).unwrap();
    let source_count: u64 = connection
        .query_row("SELECT COUNT(*) FROM source_instances", [], |row| {
            row.get(0)
        })
        .unwrap();
    let object_count: u64 = connection
        .query_row("SELECT COUNT(*) FROM source_objects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_count, 0);
    assert_eq!(object_count, 0);
    drop(connection);
    engine.shutdown().unwrap();
}

#[test]
fn every_prepared_scan_can_begin_before_any_scan_finishes() {
    const WAIT: Duration = Duration::from_secs(15);
    let temp = TempDir::new().unwrap();
    let first_root = temp.path().join("first");
    let second_root = temp.path().join("second");
    std::fs::create_dir_all(&first_root).unwrap();
    std::fs::create_dir_all(&second_root).unwrap();
    std::fs::write(
        first_root.join("settings.json"),
        br#"{"model":"claude-sonnet"}"#,
    )
    .unwrap();
    std::fs::write(
        second_root.join("settings.json"),
        br#"{"model":"claude-opus"}"#,
    )
    .unwrap();
    let first_engine = open_engine(temp.path().join("first-start.db"));
    let second_engine = open_engine(temp.path().join("second-start.db"));
    let first_gate = Arc::new(DecodeGate::default());
    let second_gate = Arc::new(DecodeGate::default());

    let first = ObservationSupervisor::prepare_with_watcher_factory(
        Arc::clone(&first_engine),
        GatedClaudeAdapter::new(Arc::clone(&first_gate)),
        ObservationSupervisorOptions::new(vec![first_root]),
        silent_watcher,
        QueryCancellationToken::default(),
    )
    .unwrap();
    let second = ObservationSupervisor::prepare_with_watcher_factory(
        Arc::clone(&second_engine),
        GatedClaudeAdapter::new(Arc::clone(&second_gate)),
        ObservationSupervisorOptions::new(vec![second_root]),
        silent_watcher,
        QueryCancellationToken::default(),
    )
    .unwrap();

    let first = first.begin().unwrap();
    first_gate.wait_until_blocked(WAIT);
    let second = second.begin().unwrap();
    second_gate.wait_until_blocked(WAIT);
    first_gate.release();
    second_gate.release();

    let mut first = first.finish().unwrap();
    let mut second = second.finish().unwrap();
    first.shutdown().unwrap();
    second.shutdown().unwrap();
    first_engine.shutdown().unwrap();
    second_engine.shutdown().unwrap();
}

#[test]
fn begun_scan_can_replace_the_request_cancellation_token() {
    const WAIT: Duration = Duration::from_secs(15);
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("settings.json"), br#"{"model":"claude-sonnet"}"#).unwrap();
    let engine = open_engine(temp.path().join("detached-start.db"));
    let gate = Arc::new(DecodeGate::default());
    let request_cancellation = QueryCancellationToken::default();
    let background_cancellation = QueryCancellationToken::default();

    let prepared = ObservationSupervisor::prepare_with_watcher_factory(
        Arc::clone(&engine),
        GatedClaudeAdapter::new(Arc::clone(&gate)),
        ObservationSupervisorOptions::new(vec![root]),
        silent_watcher,
        request_cancellation.clone(),
    )
    .unwrap();
    let starting = prepared
        .begin_with_cancellation(background_cancellation)
        .unwrap();
    gate.wait_until_blocked(WAIT);

    request_cancellation.cancel();
    gate.release();
    let mut supervisor = starting.finish().unwrap();
    assert!(supervisor.is_alive());

    supervisor.shutdown().unwrap();
    engine.shutdown().unwrap();
}

#[test]
fn startup_drains_more_than_one_wake_of_sibling_object_backlog() {
    const OBJECTS: usize = MAX_RECONCILE_PASSES_PER_WAKE + 1;
    // One record beyond the coordinator's bounded 4,096-record pass makes
    // every object publish a sibling retry target.
    const RECORDS_PER_OBJECT: usize = 4_097;
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..OBJECTS {
        std::fs::write(
            root.join(format!("{index:02}.jsonl")),
            b"{}\n".repeat(RECORDS_PER_OBJECT),
        )
        .unwrap();
    }
    let database = temp.path().join("sibling-backlog.db");
    let engine = open_engine(database);
    let prepared = ObservationSupervisor::prepare_with_watcher_factory(
        Arc::clone(&engine),
        IgnoredAppendAdapter::new(),
        ObservationSupervisorOptions::new(vec![root.clone()]),
        silent_watcher,
        QueryCancellationToken::default(),
    )
    .unwrap();
    let mut supervisor = prepared.start().unwrap();

    let catalog = engine
        .source_catalog(
            "ignored-append",
            &platform_path_key(&root.canonicalize().unwrap()),
        )
        .unwrap();
    let objects = catalog
        .objects
        .iter()
        .filter(|object| object.stream_key == "records")
        .collect::<Vec<_>>();
    assert_eq!(objects.len(), OBJECTS);
    for object in objects {
        assert_eq!(
            SourceCursor::from_opaque(object.committed_cursor.clone())
                .unwrap()
                .append_offset_value(),
            Some(u64::try_from(RECORDS_PER_OBJECT * 3).unwrap())
        );
    }
    let ready = engine.status().observation;
    assert_eq!(ready.state, "live", "{ready:?}");
    assert_eq!(ready.dirty_instances, 0, "{ready:?}");
    assert!(!ready.full_reconcile_required, "{ready:?}");

    supervisor.shutdown().unwrap();
    engine.shutdown().unwrap();
}

#[test]
fn unavailable_native_watcher_starts_in_polling_fallback_and_detects_changes() {
    const WAIT: Duration = Duration::from_secs(15);
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    let settings = root.join("settings.json");
    std::fs::write(&settings, br#"{"model":"claude-sonnet"}"#).unwrap();
    let database = temp.path().join("fallback.db");
    let engine = open_engine(database.clone());

    let prepared = ObservationSupervisor::prepare_with_watcher_factory(
        Arc::clone(&engine),
        ClaudeCodeAdapter::new(),
        ObservationSupervisorOptions::new(vec![root]),
        unavailable_watcher,
        QueryCancellationToken::default(),
    )
    .unwrap();
    let mut supervisor = prepared.start().unwrap();

    assert!(supervisor.is_alive());
    assert!(!supervisor.watcher_available());
    assert_eq!(supervisor.watch_roots(), 0);
    assert_eq!(effective_settings_model(&database), "claude-sonnet");

    std::fs::write(&settings, br#"{"model":"claude-opus"}"#).unwrap();
    wait_until(
        WAIT,
        || effective_settings_model(&database) == "claude-opus",
        || "polling fallback did not reconcile the settings change".to_string(),
    );

    supervisor.shutdown().unwrap();
    engine.shutdown().unwrap();
}

#[test]
fn overlapping_and_missing_logical_roots_consolidate_to_one_native_watch() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(root.join("projects/project")).unwrap();
    let topology =
        discover_topology(&ClaudeCodeAdapter::new(), std::slice::from_ref(&root)).unwrap();

    assert_eq!(topology.instances.len(), 1);
    assert_eq!(topology.physical_roots, vec![root.canonicalize().unwrap()]);
}

#[test]
fn path_routing_marks_only_the_affected_instance() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let topology =
        discover_topology(&ClaudeCodeAdapter::new(), &[first.clone(), second.clone()]).unwrap();
    assert_eq!(topology.instances.len(), 2);
    let first = first.canonicalize().unwrap();
    assert!(topology.instances.iter().any(|instance| instance
        .spec
        .roots
        .iter()
        .any(|root| first.join("settings.json").starts_with(&root.path))));
    let engine = open_engine(temp.path().join("route.db"));
    let event = Event::new(notify::event::EventKind::Modify(
        notify::event::ModifyKind::Any,
    ))
    .add_path(first.join("settings.json"));
    route_ingress(
        &engine,
        "claude-code",
        &topology,
        WatchIngress::Event(event),
    );

    let status = engine.status().observation;
    assert_eq!(status.dirty_instances, 1, "{status:?}");
    assert!(!status.full_reconcile_required, "{status:?}");
    match engine.next_observation_work("claude-code") {
        Some(PendingObservationWork::Object {
            stream_key,
            object_key,
            ..
        }) => {
            assert_eq!(stream_key, "interpretation-settings");
            assert_eq!(
                object_key,
                confined_relative_path_key(Path::new("settings.json")).unwrap()
            );
        }
        other => panic!("expected one object-scoped watcher route, got {other:?}"),
    }
    engine.shutdown().unwrap();
}

#[test]
fn path_routing_ignores_unselected_content_and_discovers_membership_changes() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    let topology =
        discover_topology(&ClaudeCodeAdapter::new(), std::slice::from_ref(&root)).unwrap();
    let root = root.canonicalize().unwrap();
    let engine = open_engine(temp.path().join("route-membership.db"));

    route_ingress(
        &engine,
        "claude-code",
        &topology,
        WatchIngress::Event(
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join("unrelated.tmp")),
        ),
    );
    assert!(engine.next_observation_work("claude-code").is_none());

    route_ingress(
        &engine,
        "claude-code",
        &topology,
        WatchIngress::Event(
            Event::new(EventKind::Create(notify::event::CreateKind::File))
                .add_path(root.join("settings.json")),
        ),
    );
    assert!(matches!(
        engine.next_observation_work("claude-code"),
        Some(PendingObservationWork::Instance { .. })
    ));
    engine.shutdown().unwrap();
}

#[test]
fn overflow_and_backend_errors_escalate_to_adapter_recovery() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    let topology = discover_topology(&ClaudeCodeAdapter::new(), &[root]).unwrap();
    let engine = open_engine(temp.path().join("overflow.db"));

    route_ingress(
        &engine,
        "claude-code",
        &topology,
        WatchIngress::Event(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)),
    );
    assert!(engine.status().observation.recovery_required);
    route_ingress(
        &engine,
        "claude-code",
        &topology,
        WatchIngress::BackendError,
    );
    assert!(engine.status().observation.full_reconcile_required);
    engine.shutdown().unwrap();
}

#[test]
fn polling_cadence_uses_incomplete_active_idle_and_failure_backoff() {
    let mut policy = PollingPolicy::default();
    assert!(next_poll_delay(&policy, false) >= Duration::from_secs(5));
    assert_eq!(next_poll_delay(&policy, true), WATCHER_AUDIT_INTERVAL);

    let generic_retry = DrainSummary {
        retries_required: true,
        ..DrainSummary::default()
    };
    update_polling_after_drain(&mut policy, &generic_retry);
    assert!(next_poll_delay(&policy, false) >= Duration::from_secs(5));

    let retry = DrainSummary {
        retries_required: true,
        incomplete_tail_retry: true,
        ..DrainSummary::default()
    };
    update_polling_after_drain(&mut policy, &retry);
    assert_eq!(next_poll_delay(&policy, false), Duration::from_millis(50));
    assert_eq!(next_poll_delay(&policy, true), Duration::from_millis(50));

    let active = DrainSummary {
        changed: true,
        ..DrainSummary::default()
    };
    update_polling_after_drain(&mut policy, &active);
    assert_eq!(next_poll_delay(&policy, false), Duration::from_millis(500));
    assert_eq!(next_poll_delay(&policy, true), WATCHER_AUDIT_INTERVAL);

    let failure = DrainSummary {
        watcher_failure: true,
        ..DrainSummary::default()
    };
    update_polling_after_drain(&mut policy, &failure);
    update_polling_after_drain(&mut policy, &failure);
    update_polling_after_drain(&mut policy, &failure);
    assert!(policy.fallback_active());
    assert_eq!(next_poll_delay(&policy, false), Duration::from_millis(500));
}

#[test]
fn callback_admitted_inside_initial_scan_is_reconciled_before_ready() {
    const WAIT: Duration = Duration::from_secs(15);
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    let settings = root.join("settings.json");
    std::fs::write(&settings, br#"{"model":"claude-sonnet"}"#).unwrap();
    let database = temp.path().join("race.db");
    let engine = open_engine(database.clone());
    let gate = Arc::new(DecodeGate::default());
    let adapter = GatedClaudeAdapter::new(Arc::clone(&gate));
    let starting_engine = Arc::clone(&engine);
    let starting_root = root.clone();
    let start = thread::spawn(move || {
        starting_engine.start_observation_supervisor(
            adapter,
            ObservationSupervisorOptions::new(vec![starting_root]),
        )
    });

    gate.wait_until_blocked(WAIT);
    assert!(engine.status().observation.reconcile_in_flight);
    std::fs::write(&settings, br#"{"model":"claude-opus"}"#).unwrap();
    let topology =
        discover_topology(&ClaudeCodeAdapter::new(), std::slice::from_ref(&root)).unwrap();
    route_ingress(
        &engine,
        "claude-code",
        &topology,
        WatchIngress::Event(
            Event::new(notify::event::EventKind::Modify(
                notify::event::ModifyKind::Any,
            ))
            .add_path(settings),
        ),
    );
    let during_scan = engine.status().observation;
    assert!(
        during_scan.dirty_instances == 1 || during_scan.full_reconcile_required,
        "{during_scan:?}"
    );
    gate.release();

    start.join().unwrap().unwrap();
    let ready = engine.status().observation;
    assert_eq!(ready.state, "live", "{ready:?}");
    assert_eq!(ready.dirty_instances, 0, "{ready:?}");
    assert!(ready.reconciles_total >= 2, "{ready:?}");
    assert_eq!(effective_settings_model(&database), "claude-opus");
    engine.shutdown().unwrap();
}

#[test]
fn bootstrap_pause_drains_changes_admitted_during_finalization_before_resume() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    let settings = root.join("settings.json");
    std::fs::write(&settings, br#"{"model":"claude-sonnet"}"#).unwrap();
    let database = temp.path().join("bootstrap-pause.db");
    let engine = open_engine(database.clone());
    let prepared = ObservationSupervisor::prepare_with_watcher_factory(
        Arc::clone(&engine),
        ClaudeCodeAdapter::new(),
        ObservationSupervisorOptions::new(vec![root.clone()]),
        silent_watcher,
        QueryCancellationToken::default(),
    )
    .unwrap();
    let mut supervisor = prepared.start().unwrap();
    assert_eq!(effective_settings_model(&database), "claude-sonnet");

    let paused = supervisor.client().pause_for_bootstrap().unwrap();
    std::fs::write(&settings, br#"{"model":"claude-opus"}"#).unwrap();
    let topology =
        discover_topology(&ClaudeCodeAdapter::new(), std::slice::from_ref(&root)).unwrap();
    route_ingress(
        &engine,
        "claude-code",
        &topology,
        WatchIngress::Event(
            Event::new(EventKind::Modify(notify::event::ModifyKind::Any)).add_path(settings),
        ),
    );
    assert_eq!(effective_settings_model(&database), "claude-sonnet");
    let admitted = engine.status().observation;
    assert!(
        admitted.dirty_instances == 1 || admitted.full_reconcile_required,
        "{admitted:?}"
    );

    paused.resume().unwrap();
    assert_eq!(effective_settings_model(&database), "claude-opus");
    let ready = engine.status().observation;
    assert_eq!(ready.state, "live", "{ready:?}");
    assert_eq!(ready.dirty_instances, 0, "{ready:?}");

    supervisor.shutdown().unwrap();
    engine.shutdown().unwrap();
}

#[test]
fn native_supervisor_registers_before_scan_and_refreshes_changes() {
    const WAIT: Duration = Duration::from_secs(15);
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("settings.json"), br#"{"model":"claude-sonnet"}"#).unwrap();
    let database = temp.path().join("engine.db");
    let engine = open_engine(database.clone());
    engine
        .start_observation_supervisor(
            ClaudeCodeAdapter::new(),
            ObservationSupervisorOptions::new(vec![root.clone()]),
        )
        .unwrap();

    let started = engine.status().observation;
    assert_eq!(started.state, "live");
    assert_eq!(started.supervisors_running, 1);
    assert_eq!(started.watched_instances, 1);
    assert_eq!(started.watch_roots, 1);
    assert_eq!(count_settings(&database), 1);
    assert!(engine
        .start_observation_supervisor(
            ClaudeCodeAdapter::new(),
            ObservationSupervisorOptions::new(vec![root.clone()]),
        )
        .is_err());
    // Native backends can surface bootstrap hints after registration; wait
    // for the original supervisor to quiesce before mutating the fixture.
    wait_until(
        WAIT,
        || {
            let observation = engine.status().observation;
            !observation.reconcile_in_flight
                && observation.dirty_instances == 0
                && !observation.full_reconcile_required
        },
        || format!("{:?}", engine.status().observation),
    );

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(root.join("settings.json"))
        .unwrap();
    file.write_all(br#"{"model":"claude-opus"}"#).unwrap();
    file.flush().unwrap();
    drop(file);
    // Filesystem backends are not uniformly delivered by hermetic test
    // runners (notably macOS FSEvents under a sandbox). Direct event
    // routing is covered above; this integration test exercises the same
    // running supervisor through its portable refresh control path.
    engine
        .refresh_observation_supervisor("claude-code")
        .unwrap();
    wait_until(
        WAIT,
        || {
            let observation = engine.status().observation;
            observation.reconciles_total >= 2
                && !observation.reconcile_in_flight
                && observation.state == "live"
        },
        || format!("{:?}", engine.status().observation),
    );
    assert_eq!(engine.status().observation.state, "live");
    assert_eq!(count_settings(&database), 1);

    assert!(engine.stop_observation_supervisor("claude-code").unwrap());
    assert!(!engine.stop_observation_supervisor("claude-code").unwrap());
    assert_eq!(engine.status().observation.supervisors_running, 0);

    engine
        .start_observation_supervisor(
            ClaudeCodeAdapter::new(),
            ObservationSupervisorOptions::new(vec![root]),
        )
        .unwrap();
    assert_eq!(engine.status().observation.supervisors_running, 1);
    engine.shutdown().unwrap();
    let stopped = engine.status();
    assert_eq!(stopped.state, "stopped");
    assert_eq!(stopped.observation.supervisors_running, 0);
}

#[test]
fn supervisor_restart_resumes_the_durable_append_cursor() {
    const WAIT: Duration = Duration::from_secs(15);
    const PROJECT: &str = "-Users-fixture-project";
    const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let project = root.join(format!("projects/{PROJECT}"));
    std::fs::create_dir_all(&project).unwrap();
    let transcript = project.join(format!("{SESSION}.jsonl"));
    std::fs::write(&transcript, transcript_line(SESSION, "m1", "first")).unwrap();
    let database = temp.path().join("restart.db");

    let first = open_engine(database.clone());
    first
        .start_observation_supervisor(
            ClaudeCodeAdapter::new(),
            ObservationSupervisorOptions::new(vec![root.clone()]),
        )
        .unwrap();
    assert_eq!(count_messages(&database), 1);
    let first_instance_id = source_instance_id(&database);
    let first_cursor = transcript_cursor(&first, &root);
    assert_eq!(
        first_cursor.append_offset_value(),
        Some(file_len(&transcript))
    );
    first.shutdown().unwrap();

    let mut append = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    append
        .write_all(&transcript_line(SESSION, "m2", "second"))
        .unwrap();
    append.flush().unwrap();
    drop(append);

    let restarted = open_engine(database.clone());
    restarted
        .start_observation_supervisor(
            ClaudeCodeAdapter::new(),
            ObservationSupervisorOptions::new(vec![root.clone()]),
        )
        .unwrap();
    wait_until(
        WAIT,
        || count_messages(&database) == 2,
        || format!("{:?}", restarted.status().observation),
    );
    assert_eq!(source_instance_id(&database), first_instance_id);
    let resumed_cursor = transcript_cursor(&restarted, &root);
    assert_eq!(
        resumed_cursor.append_offset_value(),
        Some(file_len(&transcript))
    );
    assert!(resumed_cursor.append_offset_value() > first_cursor.append_offset_value());
    assert_eq!(restarted.status().observation.state, "live");
    restarted.shutdown().unwrap();
}

#[derive(Default)]
struct DecodeGate {
    calls: AtomicUsize,
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    blocked: bool,
    released: bool,
}

impl DecodeGate {
    fn enter(&self) {
        if self.calls.fetch_add(1, AtomicOrdering::AcqRel) != 0 {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.blocked = true;
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn wait_until_blocked(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        while !state.blocked {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("initial reconcile did not reach the decode gate");
            let (next, timed_out) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
            assert!(!timed_out.timed_out(), "initial reconcile decode timed out");
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.released = true;
        self.changed.notify_all();
    }
}

struct GatedClaudeAdapter {
    inner: ClaudeCodeAdapter,
    gate: Arc<DecodeGate>,
}

struct IgnoredAppendAdapter {
    manifest: AdapterManifest,
}

impl IgnoredAppendAdapter {
    fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                id: AdapterId::new("ignored-append").unwrap(),
                display_name: "ignored append test adapter".to_string(),
                adapter_version: "1.0.0".to_string(),
                contract_version: 1,
                support_binding: None,
                scope_programs: None,
                source_schema_versions: Vec::new(),
                capabilities: Vec::new(),
            },
        }
    }
}

impl AgentAdapter for IgnoredAppendAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
        context
            .configured_roots
            .iter()
            .map(|root| {
                let canonical = root.canonicalize().map_err(|error| {
                    AdapterError::new(
                        AdapterErrorClass::Transient,
                        "root_unavailable",
                        error.to_string(),
                    )
                })?;
                Ok(SourceInstanceSpec {
                    identity_contract_version: 1,
                    stable_key: SourceInstanceKey::new(platform_path_key(&canonical))?,
                    display_name: "ignored append fixture".to_string(),
                    roots: vec![SourceRoot {
                        name: "root".to_string(),
                        path: canonical,
                    }],
                    discovery_reason: "supervisor sibling backlog test".to_string(),
                })
            })
            .collect()
    }

    fn streams(&self, _instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        Ok(vec![StreamSpec {
            id: StreamId::new("records")?,
            driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
            selector: ObjectSelector {
                root_name: "root".to_string(),
                include: vec!["*.jsonl".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new("ignored-jsonl-v1")?,
            authority: StreamAuthority::Canonical,
            entity_scope: EntityScope::Instance,
            priority: IngestPriority::Backfill,
            consistency: ConsistencyPolicy::IncrementalCursor,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::HashOnly,
            capabilities: Vec::new(),
        }])
    }

    fn decode(
        &self,
        _context: DecodeContext<'_>,
        _record: &SourceRecord,
        _output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        Ok(DecodeDisposition::IgnoredKnown)
    }
}

impl GatedClaudeAdapter {
    fn new(gate: Arc<DecodeGate>) -> Self {
        Self {
            inner: ClaudeCodeAdapter::new(),
            gate,
        }
    }
}

impl AgentAdapter for GatedClaudeAdapter {
    fn manifest(&self) -> &AdapterManifest {
        self.inner.manifest()
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
        self.inner.discover(context)
    }

    fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        self.inner.streams(instance)
    }

    fn bootstrap_object(
        &self,
        instance: &SourceInstance,
        object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        self.inner.bootstrap_object(instance, object)
    }

    fn decode(
        &self,
        context: DecodeContext<'_>,
        record: &SourceRecord,
        output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        self.gate.enter();
        self.inner.decode(context, record, output)
    }
}

fn open_engine(database_path: PathBuf) -> Arc<SpaghettiEngineCore> {
    SpaghettiEngineCore::open(EngineOptions {
        database_path,
        query_workers: Some(1),
        owner_label: Some("supervisor-test".to_string()),
        defer_query_structures: false,
        source_pass_pool: None,
    })
    .unwrap()
}

fn count_settings(database: &Path) -> i64 {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row(
            "SELECT COUNT(*) FROM canonical_interpretation_settings_documents",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn effective_settings_model(database: &Path) -> String {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let json: Vec<u8> = connection
        .query_row(
            "SELECT effective_settings_json FROM canonical_effective_interpretation_settings",
            [],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_slice::<serde_json::Value>(&json).unwrap()["model"]
        .as_str()
        .unwrap()
        .to_string()
}

fn transcript_line(session: &str, message_id: &str, body: &str) -> Vec<u8> {
    let mut line = format!(
            r#"{{"type":"assistant","uuid":"{message_id}","timestamp":"2026-08-12T00:00:00Z","sessionId":"{session}","cwd":"/fixture/project","version":"1","gitBranch":"main","isSidechain":false,"userType":"external","requestId":"request-{message_id}","message":{{"model":"claude-sonnet","id":"api-{message_id}","type":"message","role":"assistant","content":[{{"type":"text","text":"{body}"}}],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
        )
        .into_bytes();
    line.push(b'\n');
    line
}

fn count_messages(database: &Path) -> i64 {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM canonical_messages", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn source_instance_id(database: &Path) -> u64 {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    connection
        .query_row(
            "SELECT source_instance_id FROM source_instances LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| u64::try_from(value).unwrap())
        .unwrap()
}

fn transcript_cursor(engine: &SpaghettiEngineCore, root: &Path) -> SourceCursor {
    let stable_key = platform_path_key(&root.canonicalize().unwrap());
    let object = engine
        .source_catalog("claude-code", &stable_key)
        .unwrap()
        .objects
        .into_iter()
        .find(|object| object.stream_key == "session-transcripts")
        .unwrap();
    SourceCursor::from_opaque(object.committed_cursor).unwrap()
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().len()
}

fn wait_until(timeout: Duration, predicate: impl Fn() -> bool, diagnostic: impl Fn() -> String) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if predicate() {
        return;
    }
    panic!(
        "condition did not become true within {timeout:?}: {}",
        diagnostic()
    );
}

// ---------------------------------------------------------------------------
// Runtime fact commits: the durable writer rejects a repeated revision
// identity, so a decode that derives one twice fails the whole reconcile.
// These drive the real supervisor, decode spine, and commit path.
// ---------------------------------------------------------------------------

fn runtime_fact_rows(database: &Path) -> (i64, i64) {
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let committed: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM fact_records WHERE fact_kind LIKE 'runtime.%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let distinct: i64 = connection
        .query_row(
            "SELECT COUNT(DISTINCT semantic_fact_revision_id) FROM fact_records
             WHERE fact_kind LIKE 'runtime.%' AND semantic_fact_revision_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (committed, distinct)
}

/// A transcript whose shapes recur, written the way Claude writes one.
///
/// The repetition is the point: the same tool called with the same input, a
/// model and permission mode that oscillate, and a record repeated verbatim.
/// Every one of those produced a duplicate `FactRevisionId` before the
/// identity fix, and the durable writer answers a duplicate by failing the
/// commit — so a decode bug shows up here as a failed reconcile.
fn write_repetitive_transcript(path: &Path, turns: u32) {
    let mut lines: Vec<String> = Vec::new();
    for turn in 0..turns {
        let model = if turn % 2 == 0 { "model-a" } else { "model-b" };
        let mode = if turn % 3 == 0 {
            "default"
        } else {
            "acceptEdits"
        };
        let call = format!("toolu_{}", turn % 8);
        let assistant = serde_json::json!({
            "type": "assistant",
            "uuid": format!("{turn:08x}-0000-4000-8000-000000000000"),
            "parentUuid": null,
            "timestamp": "2026-04-01T00:00:00.000Z",
            "sessionId": "00000000-0000-4000-8000-000000000000",
            "cwd": "/w/p",
            "version": "1.0.0",
            "gitBranch": "main",
            "isSidechain": false,
            "userType": "external",
            "permissionMode": mode,
            "message": {
                "id": format!("msg_{turn}"),
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [
                    {"type": "text", "text": "the same sentence every turn"},
                    {"type": "tool_use", "id": call, "name": "Read", "input": {"path": "same"}},
                ],
                "usage": {"input_tokens": 10, "output_tokens": 5},
            },
        })
        .to_string();
        let user = serde_json::json!({
            "type": "user",
            "uuid": format!("{turn:08x}-1111-4000-8000-000000000000"),
            "parentUuid": null,
            "timestamp": "2026-04-01T00:00:00.000Z",
            "sessionId": "00000000-0000-4000-8000-000000000000",
            "cwd": "/w/p",
            "version": "1.0.0",
            "isSidechain": false,
            "userType": "external",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": call, "content": "same result"},
                ],
            },
        })
        .to_string();
        lines.push(assistant.clone());
        lines.push(user);
        if turn % 10 == 0 {
            lines.push(assistant);
        }
    }
    std::fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
}

#[test]
fn a_repetitive_corpus_commits_every_runtime_fact() {
    const WAIT: Duration = Duration::from_secs(60);
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("claude");
    let project = root.join("projects").join("-w-p");
    std::fs::create_dir_all(&project).unwrap();
    write_repetitive_transcript(
        &project.join("00000000-0000-4000-8000-000000000000.jsonl"),
        700,
    );

    let database = temp.path().join("engine.db");
    let engine = open_engine(database.clone());
    engine
        .start_observation_supervisor(
            ClaudeCodeAdapter::new(),
            ObservationSupervisorOptions::new(vec![root]),
        )
        .unwrap();
    wait_until(
        WAIT,
        || {
            let observation = engine.status().observation;
            !observation.reconcile_in_flight && observation.dirty_instances == 0
        },
        || format!("{:?}", engine.status().observation),
    );

    let observation = engine.status().observation;
    assert_eq!(
        observation.failed_reconciles_total, 0,
        "a runtime fact commit failed: {:?}",
        observation.last_error
    );
    assert_eq!(observation.last_error, None);
    engine.shutdown().unwrap();

    let (committed, distinct) = runtime_fact_rows(&database);
    assert!(
        committed > 4_000,
        "the generated corpus should commit thousands of runtime facts, got {committed}"
    );
    assert_eq!(
        committed, distinct,
        "every committed runtime fact must own its revision identity"
    );
}

/// Ingest a real native corpus and prove nothing is rejected at commit.
///
/// Ignored by default: it needs a populated native root, which only exists on
/// a developer machine. Run with
/// `CLAUDE_CORPUS_ROOT=~/.claude cargo test -p spaghetti-napi runtime_facts_commit_on_a_real_corpus -- --ignored --nocapture`.
///
/// It prints counts and durations only — never a path, name, id, or prompt.
#[test]
#[ignore = "requires a populated native root"]
fn runtime_facts_commit_on_a_real_corpus() {
    let Ok(root) = std::env::var("CLAUDE_CORPUS_ROOT") else {
        panic!("set CLAUDE_CORPUS_ROOT to a native agent root");
    };
    let root = PathBuf::from(root);
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("corpus.db");
    let engine = open_engine(database.clone());

    let started = Instant::now();
    engine
        .start_observation_supervisor(
            ClaudeCodeAdapter::new(),
            ObservationSupervisorOptions::new(vec![root]),
        )
        .unwrap();
    wait_until(
        Duration::from_secs(3_600),
        || {
            let observation = engine.status().observation;
            !observation.reconcile_in_flight && observation.dirty_instances == 0
        },
        || format!("{:?}", engine.status().observation),
    );
    let elapsed = started.elapsed();

    let observation = engine.status().observation;
    println!(
        "history: {:.1}s, reconciles={}, failed={}",
        elapsed.as_secs_f64(),
        observation.reconciles_total,
        observation.failed_reconciles_total,
    );
    assert_eq!(
        observation.failed_reconciles_total, 0,
        "a real-corpus commit failed: {:?}",
        observation.last_error
    );
    assert_eq!(observation.last_error, None, "a real-corpus commit errored");
    engine.shutdown().unwrap();

    let (committed, distinct) = runtime_fact_rows(&database);
    println!("runtime facts committed: {committed} ({distinct} distinct revisions)");
    assert!(committed > 0, "a real corpus must commit runtime facts");
    assert_eq!(
        committed, distinct,
        "every committed runtime fact must own its revision identity"
    );
}
