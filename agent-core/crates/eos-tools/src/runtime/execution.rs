//! The inner execution pipeline: reject-`background` → pre-hooks → execute (the
//! body parses) → validate output → stamp-terminal-on-success.
//!
//! Ports the inner half of `_framework/execution/tool_call.py::execute_tool_once`
//! and `_framework/core/validation.py`. The async query/dispatch loop, budget
//! counting, `ToolResultBlock`, `StreamEvent` emission, phase buffer, and trace
//! are **`eos-engine`** (this crate owns the *decisions*, the engine owns the
//! *loop*). The unexercised post-hook stage is dropped (every wired hook is a
//! pre-hook).
//!
//! ## Pre-hook input order — intentional divergence from Python (NF1)
//!
//! Python parses input into the validated model *first*, then runs pre-hooks on
//! that model (`tool_call.py:157` then `:163`). This generic pipeline instead runs
//! pre-hooks on the **raw** [`JsonObject`] and parses inside each executor body
//! (`execute` → [`parse_input`]). A hook author must know two consequences:
//!
//! 1. **Pre-hooks see raw, pre-`serde`-default input** — a `#[serde(default)]`
//!    field is observed *absent* here, where Python shows the default-applied value.
//! 2. **A pre-hook `Deny` precedes a parse error** — for input that is both
//!    malformed *and* hook-denied, this pipeline returns the `Deny`; Python returns
//!    the parse error first.
//!
//! This is behavior-equivalent for every wired hook: all read **required** fields
//! (`command`/`cmd`, `tool_name`) that carry no `serde` default, and none inspects
//! a defaulted field. The order is observable only by a hypothetical future hook
//! that reads a defaulted field — revisit then (parse-before-hooks would need a
//! per-tool "normalize raw → default-applied JSON" seam, since the loop is generic
//! over `JsonObject`). `pre_hook_denies_before_parse` locks the precedence half.

use eos_types::JsonObject;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::core::error::ToolError;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::hooks::{hook_failure_result, HookOutcome};
use crate::runtime::executor::RegisteredTool;

/// Run one tool call end-to-end (the inner pipeline).
///
/// Returns `Ok(ToolResult)` for normal execution — *including in-band tool-domain
/// errors* (bad args, hook deny, "tool said no"). Returns `Err(ToolError)` only on
/// a framework fault surfaced by the executor (`error.rs`, §8.2).
///
/// # Errors
/// Propagates a [`ToolError`] from a hook reading downstream state or from the
/// executor body.
pub async fn execute_tool_once(
    tool: &RegisteredTool,
    raw_input: &JsonObject,
    ctx: &crate::core::metadata::ExecutionMetadata,
) -> Result<ToolResult, ToolError> {
    // 1. `background` is not a tool argument (no in-scope tool accepts it).
    if raw_input.contains_key("background") {
        return Ok(ToolResult::error(format!(
            "Invalid input for {}: `background` is not a tool argument. \
             Use typed subagent or command-session controls instead.",
            tool.name.as_str()
        )));
    }

    // 2. Pre-hooks run on RAW (unparsed, pre-default) input — see the module-level
    //    NF1 note. The first Deny short-circuits to an in-band error before parse.
    if let Some(denial) = run_pre_hooks(tool, raw_input, ctx).await? {
        return Ok(denial);
    }

    // 3. Execute the body.
    let result = tool.executor().execute(raw_input, ctx).await?;

    // 4. Validate output against the declared shape.
    let result = validate_output(tool, result);

    // 5. Stamp terminal on success.
    Ok(stamp_terminal(tool, result))
}

/// Run a tool's ordered pre-hooks against `raw_input`/`ctx`. Returns
/// `Some(in-band error result)` on the first Deny (the Python
/// `_build_hook_failure_result` shape, carrying the accumulated pass-trace), or
/// `None` if every hook passes.
///
/// Shared by [`execute_tool_once`] and the engine's `ask_advisor` interception,
/// which runs the gate hooks (e.g. `BlockInIsolatedMode`) but drives the advisor
/// *execution* itself rather than calling the stub executor.
///
/// # Errors
/// Propagates a [`ToolError`] from a hook reading downstream state.
pub async fn run_pre_hooks(
    tool: &RegisteredTool,
    raw_input: &JsonObject,
    ctx: &crate::core::metadata::ExecutionMetadata,
) -> Result<Option<ToolResult>, ToolError> {
    // Passing hooks accumulate into the trace a later denial reports (the denier
    // itself is recorded in `hook_failure`, not the trace — Python parity).
    let mut hook_trace: Vec<Value> = Vec::new();
    for &hook in &tool.hooks {
        match hook.run(raw_input, ctx, &tool.hook_services).await? {
            HookOutcome::Pass(meta) => hook_trace.push(json!({
                "phase": "pre",
                "hook_name": hook.hook_name(),
                "status": "pass",
                "reason": "",
                "message": "",
                "metadata": meta,
            })),
            HookOutcome::Deny(denial) => {
                return Ok(Some(hook_failure_result(
                    hook,
                    &denial,
                    &hook_trace,
                    raw_input,
                )))
            }
        }
    }
    Ok(None)
}

/// Validate a successful result against the tool's [`OutputShape`]
/// (`validate_tool_output`). An in-band error passes through unchanged.
fn validate_output(tool: &RegisteredTool, result: ToolResult) -> ToolResult {
    if result.is_error {
        return result;
    }
    match tool.output() {
        OutputShape::Text => result,
        OutputShape::Json {
            model_name,
            validate,
        } => match validate(&result.output) {
            Ok(()) => result,
            Err(err) => {
                let mut metadata = result.metadata;
                metadata.insert("output_validation_error".to_owned(), json!(err));
                ToolResult {
                    output: format!(
                        "Invalid output from {}: output did not match {model_name}: {err}.",
                        tool.name.as_str()
                    ),
                    is_error: true,
                    metadata,
                    is_terminal: false,
                }
            }
        },
    }
}

/// Stamp `is_terminal = true` iff the tool is terminal and succeeded — the single
/// source of the loop's `TOOL_STOP` signal.
fn stamp_terminal(tool: &RegisteredTool, result: ToolResult) -> ToolResult {
    if tool.is_terminal && !result.is_error {
        ToolResult {
            is_terminal: true,
            ..result
        }
    } else {
        result
    }
}

/// Parse-and-validate raw tool input into a typed DTO, rendering the Python
/// "Invalid input for X" in-band message on failure (the executor returns the
/// `Err` value as `Ok(ToolResult)`). Constraint validation beyond serde defaults
/// lives in each DTO's own `validate` step.
///
/// # Errors
/// Returns the in-band [`ToolResult`] error when `raw` does not deserialize.
pub(crate) fn parse_input<T: DeserializeOwned>(
    tool: ToolName,
    raw: &JsonObject,
) -> Result<T, ToolResult> {
    serde_json::from_value::<T>(Value::Object(raw.clone())).map_err(|err| {
        ToolResult::error(format!(
            "Invalid input for {}: {err}. Please retry the tool call with valid arguments.",
            tool.as_str()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::core::intent::ToolIntent;
    use crate::core::metadata::ExecutionMetadata;
    use crate::hooks::Hook;
    use crate::runtime::executor::{RegisteredTool, ToolExecutor};
    use crate::support::metadata;

    #[derive(Serialize, Deserialize, schemars::JsonSchema)]
    struct Structured {
        ok: bool,
    }

    struct Canned(ToolResult);
    #[async_trait]
    impl ToolExecutor for Canned {
        async fn execute(
            &self,
            _input: &JsonObject,
            _ctx: &ExecutionMetadata,
        ) -> Result<ToolResult, ToolError> {
            Ok(self.0.clone())
        }
    }

    struct Exploding(Arc<AtomicBool>);
    #[async_trait]
    impl ToolExecutor for Exploding {
        async fn execute(
            &self,
            _input: &JsonObject,
            _ctx: &ExecutionMetadata,
        ) -> Result<ToolResult, ToolError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(ToolResult::ok("ran"))
        }
    }

    fn tool(
        name: ToolName,
        terminal: bool,
        output: OutputShape,
        result: ToolResult,
    ) -> RegisteredTool {
        RegisteredTool::new(
            name,
            ToolIntent::ReadOnly,
            terminal,
            crate::registry::spec::text_spec(name, "desc", schemars::schema_for!(Structured)),
            output,
            Arc::new(Canned(result)),
        )
    }

    // AC-tools-01: terminal stamping on success, not on error.
    #[tokio::test]
    async fn stamps_terminal_on_success() {
        let ctx = metadata();
        let ok = tool(
            ToolName::SubmitRootOutcome,
            true,
            OutputShape::Text,
            ToolResult::ok("done"),
        );
        let res = execute_tool_once(&ok, &JsonObject::new(), &ctx)
            .await
            .expect("ok");
        assert!(res.is_terminal, "successful terminal is stamped");

        let err = tool(
            ToolName::SubmitRootOutcome,
            true,
            OutputShape::Text,
            ToolResult::error("nope"),
        );
        let res = execute_tool_once(&err, &JsonObject::new(), &ctx)
            .await
            .expect("ok");
        assert!(!res.is_terminal, "errored terminal is not stamped");
    }

    // AC-tools-02: a pre-hook Deny yields an in-band hook_failure and the
    // executor never runs.
    #[tokio::test]
    async fn pre_hook_deny_short_circuits() {
        let ctx = metadata();
        let ran = Arc::new(AtomicBool::new(false));
        let exec_tool = RegisteredTool::new(
            ToolName::ExecCommand,
            ToolIntent::WriteAllowed,
            false,
            crate::registry::spec::text_spec(
                ToolName::ExecCommand,
                "desc",
                schemars::schema_for!(Structured),
            ),
            OutputShape::Text,
            Arc::new(Exploding(ran.clone())),
        )
        .with_hooks(vec![Hook::DestructiveShell {
            tool: ToolName::ExecCommand,
        }]);

        let mut input = JsonObject::new();
        input.insert(
            "cmd".to_owned(),
            Value::String("rm -rf /testbed".to_owned()),
        );
        let res = execute_tool_once(&exec_tool, &input, &ctx)
            .await
            .expect("ok");

        assert!(res.is_error, "deny is an in-band error");
        assert!(
            res.metadata.contains_key("hook_failure"),
            "carries hook_failure metadata"
        );
        assert_eq!(res.metadata["policy"], json!("destructive_shell"));
        // Python `_build_hook_failure_result` shape: the denier is the first/only
        // hook, so no hooks passed before it — the trace is present but empty, and
        // the effective input that reached the hook is echoed back.
        assert_eq!(res.metadata["hook_trace"], json!([]));
        assert_eq!(
            res.metadata["effective_tool_input"],
            json!({"cmd": "rm -rf /testbed"})
        );
        assert!(!ran.load(Ordering::SeqCst), "executor never ran");
    }

    // hook_trace records each PASSING hook (not the denier) in order — the
    // accumulate-on-pass / emit-on-deny half of the Python pipeline shape.
    #[tokio::test]
    async fn hook_trace_records_passing_hooks() {
        // No sandbox_id → BlockInIsolatedMode fails open (Pass) before the
        // DestructiveShell denial fires.
        let ctx = metadata();
        let ran = Arc::new(AtomicBool::new(false));
        let exec_tool = RegisteredTool::new(
            ToolName::ExecCommand,
            ToolIntent::WriteAllowed,
            false,
            crate::registry::spec::text_spec(
                ToolName::ExecCommand,
                "desc",
                schemars::schema_for!(Structured),
            ),
            OutputShape::Text,
            Arc::new(Exploding(ran.clone())),
        )
        .with_hooks(vec![
            Hook::BlockInIsolatedMode {
                tool: ToolName::ExecCommand,
            },
            Hook::DestructiveShell {
                tool: ToolName::ExecCommand,
            },
        ]);

        let mut input = JsonObject::new();
        input.insert(
            "cmd".to_owned(),
            Value::String("rm -rf /testbed".to_owned()),
        );
        let res = execute_tool_once(&exec_tool, &input, &ctx)
            .await
            .expect("ok");

        assert!(res.is_error);
        let trace = res.metadata["hook_trace"]
            .as_array()
            .expect("hook_trace is an array");
        assert_eq!(trace.len(), 1, "only the passing hook is traced");
        assert_eq!(
            trace[0]["hook_name"],
            json!("block_in_isolated_mode:exec_command")
        );
        assert_eq!(trace[0]["status"], json!("pass"));
        // The denier is recorded in hook_failure, not the trace.
        assert_eq!(
            res.metadata["hook_failure"]["hook_name"],
            json!("sandbox_shell:destructive_shell:exec_command")
        );
        assert!(!ran.load(Ordering::SeqCst), "executor never ran");
    }

    // AC-tools-03: parse rejects a stray `background` key.
    #[tokio::test]
    async fn rejects_background_arg() {
        let ctx = metadata();
        let any = tool(
            ToolName::ReadFile,
            false,
            OutputShape::Text,
            ToolResult::ok("x"),
        );
        let mut input = JsonObject::new();
        input.insert("background".to_owned(), json!(true));
        let res = execute_tool_once(&any, &input, &ctx).await.expect("ok");
        assert!(res.is_error);
        assert!(
            res.output.contains("`background` is not a tool argument"),
            "{}",
            res.output
        );
    }

    // AC-tools-04: output validation — text passes; structured non-matching JSON
    // is an in-band error with output_validation_error metadata; matching passes.
    #[tokio::test]
    async fn validates_output_shape() {
        let ctx = metadata();

        let text = tool(
            ToolName::ReadFile,
            false,
            OutputShape::Text,
            ToolResult::ok("free text"),
        );
        assert!(
            !execute_tool_once(&text, &JsonObject::new(), &ctx)
                .await
                .expect("ok")
                .is_error
        );

        let bad = tool(
            ToolName::ReadFile,
            false,
            OutputShape::json::<Structured>("Structured"),
            ToolResult::ok("not json"),
        );
        let res = execute_tool_once(&bad, &JsonObject::new(), &ctx)
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res.metadata.contains_key("output_validation_error"));
        assert!(
            res.output.contains("did not match Structured"),
            "{}",
            res.output
        );

        let good = tool(
            ToolName::ReadFile,
            false,
            OutputShape::json::<Structured>("Structured"),
            ToolResult::ok(r#"{"ok":true}"#),
        );
        assert!(
            !execute_tool_once(&good, &JsonObject::new(), &ctx)
                .await
                .expect("ok")
                .is_error
        );
    }

    // ---- NF1: pre-hooks run on raw input, before the body parses ----

    #[derive(Serialize, Deserialize, schemars::JsonSchema)]
    struct Needs {
        // Required field: `parse_input` fails when it is absent.
        needed: String,
    }

    // Records that the body ran, then parses `Needs` (surfacing the in-band parse
    // error like a real executor). Lets a test prove the body — and its parse — is
    // never reached when a pre-hook denies first.
    struct ParsesAndRecords(Arc<AtomicBool>);
    #[async_trait]
    impl ToolExecutor for ParsesAndRecords {
        async fn execute(
            &self,
            input: &JsonObject,
            _ctx: &ExecutionMetadata,
        ) -> Result<ToolResult, ToolError> {
            self.0.store(true, Ordering::SeqCst);
            match parse_input::<Needs>(ToolName::ExecCommand, input) {
                Ok(_) => Ok(ToolResult::ok("ran")),
                Err(in_band) => Ok(in_band),
            }
        }
    }

    // NF1: an input that is BOTH hook-denied (destructive `cmd`) AND unparseable by
    // the body's DTO (missing required `needed`). Pre-hooks run on the raw input
    // first, so the Deny wins and the body never parses — locking the documented
    // precedence (Python surfaces the parse error first; this pipeline does not).
    #[tokio::test]
    async fn pre_hook_denies_before_parse() {
        let ctx = metadata();
        let reached = Arc::new(AtomicBool::new(false));
        let exec_tool = RegisteredTool::new(
            ToolName::ExecCommand,
            ToolIntent::WriteAllowed,
            false,
            crate::registry::spec::text_spec(
                ToolName::ExecCommand,
                "desc",
                schemars::schema_for!(Needs),
            ),
            OutputShape::Text,
            Arc::new(ParsesAndRecords(reached.clone())),
        )
        .with_hooks(vec![Hook::DestructiveShell {
            tool: ToolName::ExecCommand,
        }]);

        let mut input = JsonObject::new();
        input.insert(
            "cmd".to_owned(),
            Value::String("rm -rf /testbed".to_owned()),
        );
        let res = execute_tool_once(&exec_tool, &input, &ctx)
            .await
            .expect("ok");

        assert!(res.is_error);
        assert_eq!(
            res.metadata["policy"],
            json!("destructive_shell"),
            "the hook Deny is returned, not a parse error"
        );
        assert!(
            !res.output.contains("Invalid input"),
            "the body's parse never ran: {}",
            res.output
        );
        assert!(
            !reached.load(Ordering::SeqCst),
            "the executor body (and its parse) is never reached"
        );
    }
}
