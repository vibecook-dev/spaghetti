//! Deterministic synthetic-adapter traces for the Phase 3 exit gate.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use tempfile::TempDir;

use super::*;

fn origin(object_id: u64, media_type: &str) -> RecordOrigin {
    RecordOrigin {
        source_instance_id: 1,
        stream_id: 10 + object_id,
        object_id,
        observed_at: 1_000,
        source_timestamp_hint: None,
        media_type: SourceMediaType::new(media_type).unwrap(),
    }
}

fn select_json(relative: &Path, kind: DirectoryEntryKind) -> DirectorySelection {
    match kind {
        DirectoryEntryKind::Directory => DirectorySelection::Recurse,
        DirectoryEntryKind::File if relative.extension().is_some_and(|value| value == "json") => {
            DirectorySelection::Include
        }
        DirectoryEntryKind::File => DirectorySelection::Ignore,
    }
}

#[test]
fn synthetic_adapter_converges_across_startup_restart_overflow_and_rewrite() {
    let root = TempDir::new().unwrap();
    let transcript = root.path().join("transcript.jsonl");
    let document = root.path().join("summary.json");
    let children = root.path().join("sessions");
    let presence = root.path().join("active.lock");
    std::fs::write(&transcript, b"one\npar").unwrap();
    std::fs::write(&document, b"v1").unwrap();
    std::fs::create_dir(&children).unwrap();
    std::fs::write(children.join("a.json"), b"a").unwrap();

    let append = AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap();
    let replace = ReplaceDocument::new(ReplaceDocumentConfig::default()).unwrap();
    let directory = DirectorySnapshot::new(DirectorySnapshotConfig::default()).unwrap();
    let presence_driver = PresenceObject::new(PresenceObjectConfig {
        include_content: true,
        max_content_bytes: 64,
    })
    .unwrap();
    let mut trace = Vec::new();

    let mut startup = WatchBeforeScan::new(b"synthetic".to_vec(), 4).unwrap();
    startup.watcher_registered().unwrap();
    startup.begin_scan().unwrap();
    startup
        .push_hint(DirtyHint {
            scope: DirtyScope::Object(b"transcript".to_vec()),
            reason: DirtyReason::NativeEvent,
        })
        .unwrap();

    let AppendRead::Batch {
        items,
        checkpoint: append_checkpoint,
        transition,
        needs_retry,
        ..
    } = append
        .read(&transcript, None, &origin(1, "text/plain"), false)
        .unwrap()
    else {
        panic!("append source should be stable");
    };
    let AppendItem::Record(first_record) = &items[0] else {
        panic!("first framed line should be a record");
    };
    trace.push(format!(
        "append:{transition:?}:g{}:{}:retry={needs_retry}",
        append_checkpoint.generation,
        String::from_utf8_lossy(&first_record.payload)
    ));

    let ReplaceRead::Record {
        checkpoint: replace_checkpoint,
        ..
    } = replace
        .read(&document, None, &origin(2, "application/json"), false)
        .unwrap()
    else {
        panic!("replace source should produce its first snapshot");
    };
    trace.push(format!("replace:g{}:record", replace_checkpoint.generation));

    let DirectoryScan::Snapshot {
        changes,
        checkpoint: directory_checkpoint,
        ..
    } = directory.scan(&children, None, &select_json).unwrap()
    else {
        panic!("directory source should produce its first snapshot");
    };
    trace.push(format!("directory:changes={}", changes.len()));

    let PresenceRead::Observation {
        kind,
        checkpoint: presence_checkpoint,
        ..
    } = presence_driver
        .read(&presence, None, &origin(3, "text/plain"))
        .unwrap()
    else {
        panic!("presence absence should be observable");
    };
    trace.push(format!(
        "presence:{kind:?}:g{}",
        presence_checkpoint.generation
    ));

    startup.finish_scan().unwrap();
    let StartupAction::Reconcile(hints) = startup.next_reconcile_batch(4).unwrap() else {
        panic!("hint buffered during scan must replay");
    };
    trace.push(format!("startup:reconcile={}", hints.len()));
    assert_eq!(
        startup.finish_reconcile(4).unwrap(),
        StartupAction::Live { commit_seq: 4 }
    );

    // Simulated restart proves every driver checkpoint survives its opaque
    // encoding and resumes at the committed boundary.
    let append_checkpoint = AppendCheckpoint::decode(&append_checkpoint.encode()).unwrap();
    let replace_checkpoint = ReplaceCheckpoint::decode(&replace_checkpoint.encode()).unwrap();
    let directory_checkpoint = DirectoryCheckpoint::decode(&directory_checkpoint.encode()).unwrap();
    let presence_checkpoint = PresenceCheckpoint::decode(&presence_checkpoint.encode()).unwrap();

    let mut transcript_append = OpenOptions::new().append(true).open(&transcript).unwrap();
    transcript_append.write_all(b"tial\n").unwrap();
    transcript_append.flush().unwrap();
    let AppendRead::Batch {
        items,
        checkpoint: append_checkpoint,
        transition,
        ..
    } = append
        .read(
            &transcript,
            Some(&append_checkpoint),
            &origin(1, "text/plain"),
            false,
        )
        .unwrap()
    else {
        panic!("completed tail should frame after restart");
    };
    let AppendItem::Record(completed_tail) = &items[0] else {
        panic!("completed tail should be an ordinary record");
    };
    trace.push(format!(
        "append:{transition:?}:g{}:{}",
        append_checkpoint.generation,
        String::from_utf8_lossy(&completed_tail.payload)
    ));

    let replacement_path = root.path().join("summary.next");
    std::fs::write(&replacement_path, b"v2").unwrap();
    std::fs::rename(replacement_path, &document).unwrap();
    let ReplaceRead::Record {
        checkpoint: replace_checkpoint,
        generation_changed,
        ..
    } = replace
        .read(
            &document,
            Some(&replace_checkpoint),
            &origin(2, "application/json"),
            false,
        )
        .unwrap()
    else {
        panic!("atomic document replacement should produce a snapshot");
    };
    trace.push(format!(
        "replace:g{}:generation_changed={generation_changed}",
        replace_checkpoint.generation
    ));

    std::fs::remove_file(children.join("a.json")).unwrap();
    std::fs::write(children.join("b.json"), b"b").unwrap();
    let DirectoryScan::Snapshot { changes, .. } = directory
        .scan(&children, Some(&directory_checkpoint), &select_json)
        .unwrap()
    else {
        panic!("directory reconcile should remain available");
    };
    trace.push(format!("directory:changes={}", changes.len()));

    std::fs::write(&presence, b"active").unwrap();
    let PresenceRead::Observation {
        kind,
        checkpoint: present_checkpoint,
        ..
    } = presence_driver
        .read(
            &presence,
            Some(&presence_checkpoint),
            &origin(3, "text/plain"),
        )
        .unwrap()
    else {
        panic!("presence creation should be observable");
    };
    trace.push(format!(
        "presence:{kind:?}:g{}",
        present_checkpoint.generation
    ));

    let mut dirty = DirtyCoalescer::new(b"synthetic".to_vec(), 2).unwrap();
    dirty.enqueue(DirtyHint {
        scope: DirtyScope::Object(b"one".to_vec()),
        reason: DirtyReason::NativeEvent,
    });
    dirty.enqueue(DirtyHint {
        scope: DirtyScope::Object(b"two".to_vec()),
        reason: DirtyReason::NativeEvent,
    });
    dirty.enqueue(DirtyHint {
        scope: DirtyScope::Object(b"three".to_vec()),
        reason: DirtyReason::NativeEvent,
    });
    let overflow = dirty.drain(2);
    trace.push(format!("overflow:{:?}", overflow[0].reason));
    assert_eq!(
        overflow[0].scope,
        DirtyScope::Instance(b"synthetic".to_vec())
    );

    std::fs::write(&transcript, b"reset\n").unwrap();
    let AppendRead::Batch {
        checkpoint: rewritten,
        transition,
        ..
    } = append
        .read(
            &transcript,
            Some(&append_checkpoint),
            &origin(1, "text/plain"),
            false,
        )
        .unwrap()
    else {
        panic!("truncate/rewrite should reconcile");
    };
    trace.push(format!("rewrite:{transition:?}:g{}", rewritten.generation));

    assert_eq!(
        trace,
        [
            "append:Initial:g1:one:retry=true",
            "replace:g1:record",
            "directory:changes=1",
            "presence:InitialAbsent:g1",
            "startup:reconcile=1",
            "append:Continued:g1:partial",
            "replace:g1:generation_changed=false",
            "directory:changes=2",
            "presence:Created:g2",
            "overflow:InternalQueueOverflow",
            "rewrite:Truncated:g2",
        ]
    );
}
