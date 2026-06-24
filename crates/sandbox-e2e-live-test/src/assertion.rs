use std::cell::Cell;

use serde_json::Value;

use crate::cli_client::CallRecord;

thread_local! {
    static ASSERTIONS: Cell<u64> = const { Cell::new(0) };
}

/// Total assertions invoked on the current test thread. libtest runs one thread
/// per test, so this counts the calling test's assertions without any reset.
/// Read by `Sandbox::drop` to populate `result.json`'s `assertions.total`.
#[must_use]
pub fn assertion_count() -> u64 {
    ASSERTIONS.with(Cell::get)
}

fn bump() {
    ASSERTIONS.with(|count| count.set(count.get() + 1));
}

/// Assert there is no top-level `error` key (the success discriminator).
pub fn ok(resp: &Value) {
    bump();
    assert!(
        resp.get("error").is_none(),
        "expected a success response, got error: {resp}"
    );
}

/// JSON-pointer get-or-panic. `field(resp, "/status")`, `field(resp, "/id")`, etc.
#[must_use]
pub fn field<'a>(resp: &'a Value, ptr: &str) -> &'a Value {
    bump();
    resp.pointer(ptr)
        .unwrap_or_else(|| panic!("missing field {ptr} in response: {resp}"))
}

/// Assert the carried response is an error with `error.kind == kind` and
/// `rec.exit_code == exit`. Reads `rec.response()` (parsed from the carrier
/// stream, `cli_client.rs`); the error object is `{kind,message,details}`. Stage 1
/// manager semantic errors route to exit 1 / stderr; `exit` is a parameter so the
/// helper also covers the exit-2 usage routing of the deferred Stage 2 N2 leaf.
pub fn err_kind_at(rec: &CallRecord, kind: &str, exit: i32) {
    bump();
    let resp = rec.response();
    let actual = resp
        .pointer("/error/kind")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an error response with /error/kind, got: {resp}"));
    assert_eq!(actual, kind, "unexpected error kind (response: {resp})");
    assert_eq!(
        rec.exit_code, exit,
        "unexpected exit code (response: {resp})"
    );
}
