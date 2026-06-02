// AC-workspace-08: the canonical SQLite schema snapshot lists the seven target
// tables and the three unique constraints. eos-db's versioned migrations target
// this clean shape.

use std::fs;
use std::path::Path;

fn schema_sql() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sqlite")
        .join("schema.sql");
    fs::read_to_string(&path).expect("read sqlite/schema.sql")
}

#[test]
fn seven_target_tables_present() {
    let sql = schema_sql();
    let tables = [
        "requests",
        "tasks",
        "workflows",
        "iterations",
        "attempts",
        "agent_runs",
        "model_registrations",
    ];
    for table in tables {
        let needle = format!("CREATE TABLE {table} (");
        assert!(sql.contains(&needle), "missing table {table}");
    }
    let create_count = sql.matches("CREATE TABLE ").count();
    assert_eq!(create_count, tables.len(), "unexpected table count");
}

#[test]
fn three_unique_constraints_present() {
    let sql = schema_sql();
    // Named composite unique constraints on iterations and attempts.
    assert!(
        sql.contains("uq_iteration_workflow_sequence"),
        "iterations(workflow_id, sequence_no) unique constraint"
    );
    assert!(
        sql.contains("uq_attempt_iteration_sequence"),
        "attempts(iteration_id, attempt_sequence_no) unique constraint"
    );
    // agent_runs.task_id uniqueness manifests as a unique index.
    assert!(
        sql.contains("CREATE UNIQUE INDEX ix_agent_runs_task_id ON agent_runs (task_id)"),
        "agent_runs.task_id unique constraint"
    );
}
