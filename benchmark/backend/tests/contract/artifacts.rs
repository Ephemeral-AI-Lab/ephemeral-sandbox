use std::fs::{self, OpenOptions};
use std::io::{self, Write};

use sandbox_benchmark::artifacts::{
    ArtifactError, ArtifactId, ArtifactStore, SchemaEnvelope, BOUNDED_EVIDENCE_SCHEMA_NAME,
    BOUNDED_EVIDENCE_SCHEMA_VERSION,
};
use sandbox_benchmark::model::{PhaseCorrelationRule, PhaseId, PhaseSource, PhaseUnit};
use sandbox_benchmark::scheduler::{
    ObservationRecord, PhaseStatus, SequencedObservation, OBSERVATION_SCHEMA_NAME,
    OBSERVATION_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};

use crate::support::TestRoot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Payload {
    value: String,
}

const OLDEST_OBSERVATION_FIXTURE: &[u8] =
    include_bytes!("../fixtures/artifacts/observations-v1.ndjson");
const PREVIOUS_OBSERVATION_FIXTURE: &[u8] =
    include_bytes!("../fixtures/artifacts/observations-v2.ndjson");
const PREVIOUS_TRIAL_OBSERVATION_FIXTURE: &[u8] =
    include_bytes!("../fixtures/artifacts/observations-v2-trial.ndjson");
const CURRENT_OBSERVATION_FIXTURE: &[u8] =
    include_bytes!("../fixtures/artifacts/observations-v3.ndjson");

#[test]
fn immutable_artifacts_cannot_be_overwritten() {
    let test_root = TestRoot::new("artifacts-immutable");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    store.create_run("run-1").expect("create run artifacts");
    let original = Payload {
        value: "original".to_owned(),
    };
    store
        .write_immutable(
            "run-1",
            ArtifactId::RunManifest,
            "run_manifest",
            1,
            &original,
        )
        .expect("write immutable artifact");
    let before = store
        .content("run-1", "run_manifest")
        .expect("read immutable artifact")
        .bytes;

    let error = store
        .write_immutable(
            "run-1",
            ArtifactId::RunManifest,
            "run_manifest",
            1,
            &Payload {
                value: "replacement".to_owned(),
            },
        )
        .expect_err("second immutable write must fail");
    assert!(matches!(
        error,
        ArtifactError::Io { source, .. } if source.kind() == io::ErrorKind::AlreadyExists
    ));

    let after = store
        .content("run-1", "run_manifest")
        .expect("read immutable artifact after failed overwrite")
        .bytes;
    assert_eq!(after, before);
    let decoded: Payload = store
        .read_envelope("run-1", ArtifactId::RunManifest, "run_manifest", 1)
        .expect("decode immutable artifact");
    assert_eq!(decoded, original);
}

#[test]
fn artifact_index_and_downloads_are_strictly_allowlisted() {
    let test_root = TestRoot::new("artifacts-allowlist");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    store
        .write_immutable(
            "run-1",
            ArtifactId::Summary,
            "summary",
            1,
            &Payload {
                value: "visible".to_owned(),
            },
        )
        .expect("write allowlisted summary");
    fs::write(run.join("secret.txt"), b"not allowlisted").expect("write unlisted file");

    let index = store.index("run-1").expect("index artifacts");
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].id, ArtifactId::Summary);
    assert_eq!(index[0].file_name, "summary.json");
    assert!(index[0].downloadable);

    assert!(matches!(
        store.content("run-1", "secret"),
        Err(ArtifactError::UnknownArtifact(id)) if id == "secret"
    ));
    assert!(matches!(
        store.content("run-1", "../secret.txt"),
        Err(ArtifactError::UnknownArtifact(id)) if id == "../secret.txt"
    ));
    assert!(matches!(
        store.create_run("../escape"),
        Err(ArtifactError::InvalidRunId(id)) if id == "../escape"
    ));
    assert_eq!(
        fs::read(run.join("secret.txt")).expect("read unlisted file"),
        b"not allowlisted"
    );
}

#[test]
fn trial_evidence_is_content_addressed_immutable_and_opaquely_downloadable() {
    let test_root = TestRoot::new("artifacts-bounded-evidence");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    let payload = Payload {
        value: "bounded operation evidence".to_owned(),
    };

    let reference = store
        .write_trial_evidence("run-1", "sha256:cell", "trial-000001", &payload)
        .expect("write trial evidence");
    assert!(reference.artifact_id.starts_with("bounded_evidence_"));
    assert_eq!(reference.media_type, "application/json");
    assert!(reference.sha256.starts_with("sha256:"));

    let repeated = store
        .write_trial_evidence("run-1", "sha256:cell", "trial-000001", &payload)
        .expect("idempotently retain identical evidence");
    assert_eq!(repeated, reference);
    let entries = store
        .index("run-1")
        .expect("index bounded evidence")
        .into_iter()
        .filter(|entry| entry.id == ArtifactId::BoundedEvidence)
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].artifact_id, reference.artifact_id);
    assert!(entries[0]
        .file_name
        .starts_with("cells/sha256:cell/trials/trial-000001/bounded-evidence/operation-evidence-"));
    assert_eq!(entries[0].bytes, reference.size_bytes);
    assert_eq!(entries[0].sha256, reference.sha256);

    let content = store
        .content("run-1", &reference.artifact_id)
        .expect("download by opaque id");
    assert_eq!(content.id, ArtifactId::BoundedEvidence);
    assert_eq!(content.artifact_id, reference.artifact_id);
    let envelope: SchemaEnvelope<Payload> =
        serde_json::from_slice(&content.bytes).expect("decode evidence envelope");
    assert_eq!(envelope.schema_name, BOUNDED_EVIDENCE_SCHEMA_NAME);
    assert_eq!(envelope.schema_version, BOUNDED_EVIDENCE_SCHEMA_VERSION);
    assert_eq!(envelope.data, payload);

    let relative = &entries[0].file_name;
    fs::write(run.join(relative), b"tampered").expect("tamper with evidence for conflict test");
    assert!(matches!(
        store.write_trial_evidence("run-1", "sha256:cell", "trial-000001", &payload),
        Err(ArtifactError::ImmutableArtifactConflict(_))
    ));
}

#[test]
fn trial_evidence_rejects_path_components_and_category_downloads() {
    let test_root = TestRoot::new("artifacts-bounded-evidence-paths");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    store.create_run("run-1").expect("create run artifacts");
    let payload = Payload {
        value: "safe".to_owned(),
    };
    for invalid in ["", ".", "..", "../escape", "slash/value", "unicode-λ"] {
        assert!(matches!(
            store.write_trial_evidence("run-1", invalid, "trial-1", &payload),
            Err(ArtifactError::InvalidArtifactComponent(value)) if value == invalid
        ));
    }
    assert!(matches!(
        store.content("run-1", "bounded_evidence"),
        Err(ArtifactError::UnknownArtifact(id)) if id == "bounded_evidence"
    ));
    assert!(matches!(
        store.content("run-1", "bounded_evidence_not-a-digest"),
        Err(ArtifactError::UnknownArtifact(id)) if id == "bounded_evidence_not-a-digest"
    ));
}

#[cfg(unix)]
#[test]
fn trial_evidence_rejects_symlinked_authority_directories() {
    use std::os::unix::fs::symlink;

    let test_root = TestRoot::new("artifacts-bounded-evidence-symlink");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    let outside = test_root.join("outside");
    fs::create_dir(&outside).expect("create outside directory");
    symlink(&outside, run.join("cells")).expect("substitute cells symlink");

    assert!(matches!(
        store.write_trial_evidence(
            "run-1",
            "sha256:cell",
            "trial-000001",
            &Payload {
                value: "must not escape".to_owned(),
            },
        ),
        Err(ArtifactError::UnknownArtifact(id)) if id == "cells"
    ));
    assert_eq!(
        fs::read_dir(&outside)
            .expect("read outside directory")
            .count(),
        0
    );
}

#[test]
fn artifact_readers_enforce_schema_name_and_version() {
    let test_root = TestRoot::new("artifacts-schema");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    store.create_run("run-1").expect("create run artifacts");
    store
        .write_immutable(
            "run-1",
            ArtifactId::DefinitionSnapshot,
            "definition_snapshot",
            1,
            &Payload {
                value: "definitions".to_owned(),
            },
        )
        .expect("write definition snapshot");

    let wrong_name = store
        .read_envelope::<Payload>("run-1", ArtifactId::DefinitionSnapshot, "other_schema", 1)
        .expect_err("schema name mismatch must fail");
    assert!(matches!(
        wrong_name,
        ArtifactError::SchemaMismatch { expected, actual }
            if expected == "other_schema" && actual == "definition_snapshot"
    ));

    let wrong_version = store
        .read_envelope::<Payload>(
            "run-1",
            ArtifactId::DefinitionSnapshot,
            "definition_snapshot",
            2,
        )
        .expect_err("unsupported schema version must fail");
    assert!(matches!(
        wrong_version,
        ArtifactError::UnsupportedSchema { schema, version }
            if schema == "definition_snapshot" && version == 1
    ));
}

#[test]
fn oldest_observation_fixture_migrates_without_rewriting_raw_evidence() {
    let test_root = TestRoot::new("artifacts-observation-v1");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    let path = run.join(ArtifactId::Observations.file_name());
    fs::write(&path, OLDEST_OBSERVATION_FIXTURE).expect("write oldest observation fixture");
    let before = fs::read(&path).expect("read fixture before migration");
    let before_hash = store
        .index("run-1")
        .expect("index oldest fixture")
        .into_iter()
        .find(|entry| entry.id == ArtifactId::Observations)
        .expect("observation index entry")
        .sha256;

    let recovered = store
        .read_records_recovering::<SequencedObservation>(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
        )
        .expect("decode oldest supported observation");
    assert_eq!(recovered.records.len(), 1);
    assert!(recovered.partial_tail.is_none());
    let ObservationRecord::Phase(phase) = &recovered.records[0].record else {
        panic!("v1 phase must remain a phase after migration");
    };
    assert_eq!(phase.id, PhaseId::LayerstackCommit);
    assert_eq!(phase.semantic_revision, 1);
    assert_eq!(phase.unit, PhaseUnit::Nanoseconds);
    assert_eq!(phase.source, PhaseSource::ProductTrace);
    assert_eq!(
        phase.correlation,
        PhaseCorrelationRule::ExactRequestTraceSpan
    );
    assert_eq!(phase.trace_span_name, "layerstack.squash.commit");
    assert_eq!(phase.cell_id, "cell-layerstack");
    assert_eq!(phase.trial_id, "trial-1");
    assert_eq!(phase.request_id.as_deref(), Some("request-1"));
    assert_eq!(phase.start_offset_ns, 120);
    assert_eq!(phase.duration_ns, 45);
    assert_eq!(phase.status, PhaseStatus::Succeeded);

    assert_eq!(
        fs::read(&path).expect("read fixture after migration"),
        before
    );
    let after_hash = store
        .index("run-1")
        .expect("index migrated fixture")
        .into_iter()
        .find(|entry| entry.id == ArtifactId::Observations)
        .expect("observation index entry")
        .sha256;
    assert_eq!(after_hash, before_hash);
}

#[test]
fn previous_observation_fixture_migrates_without_rewriting_raw_evidence() {
    let test_root = TestRoot::new("artifacts-observation-v2");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    let path = run.join(ArtifactId::Observations.file_name());
    fs::write(&path, PREVIOUS_OBSERVATION_FIXTURE).expect("write previous observation fixture");
    let before = fs::read(&path).expect("read fixture before migration");

    let decoded = store
        .read_records::<SequencedObservation>(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
        )
        .expect("decode previous supported observation");
    assert_eq!(decoded.len(), 1);
    assert!(matches!(decoded[0].record, ObservationRecord::Phase(_)));
    assert_eq!(
        fs::read(&path).expect("read fixture after migration"),
        before
    );
}

#[test]
fn previous_trial_observation_migrates_with_an_explicit_empty_artifact_set() {
    let test_root = TestRoot::new("artifacts-observation-v2-trial");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    let path = run.join(ArtifactId::Observations.file_name());
    fs::write(&path, PREVIOUS_TRIAL_OBSERVATION_FIXTURE)
        .expect("write previous trial observation fixture");

    let decoded = store
        .read_records::<SequencedObservation>(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
        )
        .expect("decode previous trial observation");
    let ObservationRecord::Trial(trial) = &decoded[0].record else {
        panic!("previous trial record must remain a trial");
    };
    assert!(trial.artifacts.is_empty());

    let incorrectly_promoted = String::from_utf8(PREVIOUS_TRIAL_OBSERVATION_FIXTURE.to_vec())
        .expect("fixture is UTF-8")
        .replace("\"schema_version\":2", "\"schema_version\":3");
    fs::write(&path, incorrectly_promoted).expect("write invalid current trial fixture");
    assert!(matches!(
        store.read_records::<SequencedObservation>(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
        ),
        Err(ArtifactError::Json { .. })
    ));
}

#[test]
fn observation_reader_accepts_current_and_rejects_unknown_versions() {
    let test_root = TestRoot::new("artifacts-observation-versions");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    fs::write(
        run.join(ArtifactId::Observations.file_name()),
        CURRENT_OBSERVATION_FIXTURE,
    )
    .expect("write current observation fixture");
    let decoded = store
        .read_records::<SequencedObservation>(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
        )
        .expect("decode current observation");
    assert_eq!(decoded.len(), 1);

    for version in [0, OBSERVATION_SCHEMA_VERSION + 1] {
        let bytes = format!(
            "{{\"schema_name\":\"{OBSERVATION_SCHEMA_NAME}\",\"schema_version\":{version},\"data\":{{}}}}\n"
        );
        fs::write(run.join(ArtifactId::Observations.file_name()), bytes)
            .expect("replace observation version fixture");
        assert!(matches!(
            store.read_records::<SequencedObservation>(
                "run-1",
                ArtifactId::Observations,
                OBSERVATION_SCHEMA_NAME,
                OBSERVATION_SCHEMA_VERSION,
            ),
            Err(ArtifactError::UnsupportedSchema { schema, version: actual })
                if schema == OBSERVATION_SCHEMA_NAME && actual == version
        ));
    }
}

#[test]
fn v1_observation_decoder_rejects_unknown_fields_and_phase_definition_mismatch() {
    let test_root = TestRoot::new("artifacts-observation-v1-strict");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    let path = run.join(ArtifactId::Observations.file_name());
    let unknown_field = String::from_utf8(OLDEST_OBSERVATION_FIXTURE.to_vec())
        .expect("fixture is UTF-8")
        .replace(
            "\"status\":\"succeeded\"",
            "\"status\":\"succeeded\",\"extra\":true",
        );
    fs::write(&path, unknown_field).expect("write unknown-field v1 fixture");
    assert!(matches!(
        store.read_records::<SequencedObservation>(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
        ),
        Err(ArtifactError::Json { .. })
    ));

    let wrong_revision = String::from_utf8(OLDEST_OBSERVATION_FIXTURE.to_vec())
        .expect("fixture is UTF-8")
        .replace("\"semantic_revision\":1", "\"semantic_revision\":99");
    fs::write(&path, wrong_revision).expect("write mismatched phase fixture");
    let error = store
        .read_records::<SequencedObservation>(
            "run-1",
            ArtifactId::Observations,
            OBSERVATION_SCHEMA_NAME,
            OBSERVATION_SCHEMA_VERSION,
        )
        .expect_err("mismatched v1 phase definition must fail");
    assert!(matches!(error, ArtifactError::Json { .. }));
    assert!(error.to_string().contains("revision 99"));
}

#[test]
fn ndjson_reader_rejects_a_partial_trailing_record() {
    let test_root = TestRoot::new("artifacts-ndjson");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    for value in ["one", "two"] {
        store
            .append_record(
                "run-1",
                ArtifactId::Events,
                "test_event",
                1,
                &Payload {
                    value: value.to_owned(),
                },
            )
            .expect("append complete NDJSON record");
    }
    let records: Vec<Payload> = store
        .read_records("run-1", ArtifactId::Events, "test_event", 1)
        .expect("read complete NDJSON records");
    assert_eq!(records.len(), 2);

    OpenOptions::new()
        .append(true)
        .open(run.join(ArtifactId::Events.file_name()))
        .expect("open event stream")
        .write_all(br#"{"schema_name":"test_event""#)
        .expect("write partial trailing record");

    let error = store
        .read_records::<Payload>("run-1", ArtifactId::Events, "test_event", 1)
        .expect_err("partial NDJSON tail must fail");
    assert!(matches!(
        error,
        ArtifactError::PartialNdjsonTail { line: 3, .. }
    ));
    assert!(matches!(
        store.append_record(
            "run-1",
            ArtifactId::Summary,
            "summary",
            1,
            &Payload {
                value: "invalid stream".to_owned(),
            },
        ),
        Err(ArtifactError::UnknownArtifact(id)) if id == "summary"
    ));
}

#[test]
fn restart_recovery_quarantines_the_partial_tail_before_append_resumes() {
    let test_root = TestRoot::new("artifacts-ndjson-quarantine");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    let run = store.create_run("run-1").expect("create run artifacts");
    store
        .append_record(
            "run-1",
            ArtifactId::Events,
            "test_event",
            1,
            &Payload {
                value: "complete".to_owned(),
            },
        )
        .expect("append complete event");
    let complete = fs::read(run.join(ArtifactId::Events.file_name())).expect("read complete log");
    let partial = br#"{"schema_name":"test_event","schema_version":1,"data":{"value":"torn""#;
    OpenOptions::new()
        .append(true)
        .open(run.join(ArtifactId::Events.file_name()))
        .expect("open event stream")
        .write_all(partial)
        .expect("write torn event");

    let quarantined = store
        .quarantine_partial_tail("run-1", ArtifactId::Events)
        .expect("quarantine torn tail")
        .expect("partial tail metadata");
    assert_eq!(quarantined.artifact, ArtifactId::Events);
    assert_eq!(quarantined.line, 2);
    assert_eq!(quarantined.bytes, partial.len() as u64);
    assert!(quarantined.sha256.starts_with("sha256:"));
    assert_eq!(
        fs::read(run.join(ArtifactId::Events.file_name())).expect("read repaired log"),
        complete
    );
    assert_eq!(
        fs::read(
            run.join(".recovery-quarantine")
                .join(&quarantined.file_name)
        )
        .expect("read quarantine"),
        partial
    );
    assert!(store
        .quarantine_partial_tail("run-1", ArtifactId::Events)
        .expect("complete log is unchanged")
        .is_none());

    store
        .append_record(
            "run-1",
            ArtifactId::Events,
            "test_event",
            1,
            &Payload {
                value: "after-recovery".to_owned(),
            },
        )
        .expect("append after recovery");
    let records: Vec<Payload> = store
        .read_records("run-1", ArtifactId::Events, "test_event", 1)
        .expect("read repaired stream");
    assert_eq!(
        records,
        vec![
            Payload {
                value: "complete".to_owned()
            },
            Payload {
                value: "after-recovery".to_owned()
            }
        ]
    );
    assert!(store
        .index("run-1")
        .expect("index artifacts")
        .iter()
        .all(|entry| !entry.file_name.contains("partial")));
}
