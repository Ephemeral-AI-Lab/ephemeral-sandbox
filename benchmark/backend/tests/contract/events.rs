use sandbox_benchmark::artifacts::{ArtifactId, ArtifactStore};
use sandbox_benchmark::events::{
    encode_sse, EventData, EventJournal, EventRecord, RunState, EVENT_SCHEMA_NAME,
    EVENT_SCHEMA_VERSION,
};

use crate::support::TestRoot;

#[test]
fn run_state_wire_values_cover_the_versioned_lifecycle() {
    let states = [
        (RunState::Planned, "planned"),
        (RunState::Queued, "queued"),
        (RunState::Preparing, "preparing"),
        (RunState::Running, "running"),
        (RunState::Verifying, "verifying"),
        (RunState::TearingDown, "tearing_down"),
        (RunState::Cancelling, "cancelling"),
        (RunState::Completed, "completed"),
        (RunState::Failed, "failed"),
        (RunState::Cancelled, "cancelled"),
    ];

    for (state, wire) in states {
        assert_eq!(
            serde_json::to_string(&state).expect("serialize run state"),
            format!("\"{wire}\"")
        );
        assert_eq!(
            serde_json::from_str::<RunState>(&format!("\"{wire}\""))
                .expect("deserialize run state"),
            state
        );
    }
}

#[tokio::test]
async fn events_are_persisted_before_broadcast_and_survive_replay() {
    let test_root = TestRoot::new("events-persistence");
    let store = ArtifactStore::new(&test_root.join("results")).expect("create artifact store");
    store.create_run("run-1").expect("create run artifacts");
    let journal = EventJournal::open(store.clone(), "run-1")
        .await
        .expect("open event journal");
    let mut subscriber = journal.subscribe();

    let first = journal
        .emit(
            11,
            EventData::RunState {
                state: RunState::Running,
            },
        )
        .await
        .expect("emit first event");
    assert_eq!(first.sequence, 1);

    let persisted: Vec<EventRecord> = store
        .read_records(
            "run-1",
            ArtifactId::Events,
            EVENT_SCHEMA_NAME,
            EVENT_SCHEMA_VERSION,
        )
        .expect("read persisted event before receiving broadcast");
    assert_eq!(persisted, vec![first.clone()]);
    assert_eq!(
        journal.replay_after(0).await.expect("replay first event"),
        vec![first.clone()]
    );
    assert_eq!(
        subscriber.recv().await.expect("receive broadcast event"),
        first
    );

    let reopened = EventJournal::open(store.clone(), "run-1")
        .await
        .expect("reopen event journal from persisted stream");
    let second = reopened
        .emit(
            29,
            EventData::Warning {
                code: "bounded_warning".to_owned(),
                message: "second event".to_owned(),
            },
        )
        .await
        .expect("emit event after reopen");
    assert_eq!(second.sequence, 2);
    assert_eq!(
        reopened.replay_after(1).await.expect("replay after cursor"),
        vec![second.clone()]
    );

    let sse = encode_sse(&second).expect("encode SSE event");
    assert!(sse.starts_with("id: 2\nevent: warning\ndata: "));
    assert!(sse.ends_with("\n\n"));
    let data = sse
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("SSE data field");
    let decoded: EventRecord = serde_json::from_str(data).expect("decode SSE data");
    assert_eq!(decoded, second);
}
