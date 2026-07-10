use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sandbox_mcp::catalog::selected_catalog;
use sandbox_mcp::config::OperationSet;
use sandbox_protocol::{ArgKind, CliOperationSpecDocument};
use serde_json::{json, Map, Value};

const GATEWAY_AUTH_TOKEN: &str = "mcp-test-token";
const GATEWAY_AUTH_FIELD: &str = "_sandbox_gateway_auth_token";

struct McpProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpProcess {
    fn start(set: &str, gateway_addr: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sandbox-mcp"))
            .args([
                "--set",
                set,
                "--gateway-socket",
                gateway_addr,
                "--gateway-auth-token",
                GATEWAY_AUTH_TOKEN,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start sandbox-mcp");
        let stdin = child.stdin.take().expect("sandbox-mcp stdin");
        let stdout = BufReader::new(child.stdout.take().expect("sandbox-mcp stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn start_initialized(set: &str, gateway_addr: &str) -> Self {
        let mut process = Self::start(set, gateway_addr);
        let initialized = process.request(
            "initialize",
            Some(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "sandbox-mcp-test", "version": "1"}
            })),
        );
        let initialization = result(&initialized);
        assert_eq!(initialization["protocolVersion"], "2025-06-18");
        assert_eq!(initialization["serverInfo"]["name"], "sandbox-mcp");
        assert!(initialization["serverInfo"]["version"].is_string());
        let capability_names = initialization["capabilities"]
            .as_object()
            .expect("server capabilities")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(capability_names, ["tools"]);

        process.notify("notifications/initialized", None);
        let ping = process.request("ping", None);
        assert_eq!(result(&ping), &json!({}));
        process
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = Map::from_iter([
            ("jsonrpc".to_owned(), Value::String("2.0".to_owned())),
            ("id".to_owned(), Value::from(id)),
            ("method".to_owned(), Value::String(method.to_owned())),
        ]);
        if let Some(params) = params {
            request.insert("params".to_owned(), params);
        }
        self.send(Value::Object(request));
        let response = self.read();
        assert_eq!(response["jsonrpc"], "2.0", "{response}");
        assert_eq!(response["id"], id, "{response}");
        response
    }

    fn notify(&mut self, method: &str, params: Option<Value>) {
        let mut notification = Map::from_iter([
            ("jsonrpc".to_owned(), Value::String("2.0".to_owned())),
            ("method".to_owned(), Value::String(method.to_owned())),
        ]);
        if let Some(params) = params {
            notification.insert("params".to_owned(), params);
        }
        self.send(Value::Object(notification));
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            Some(json!({"name": name, "arguments": arguments})),
        )
    }

    fn send(&mut self, message: Value) {
        serde_json::to_writer(&mut self.stdin, &message).expect("write MCP JSON");
        self.stdin.write_all(b"\n").expect("terminate MCP JSON");
        self.stdin.flush().expect("flush MCP JSON");
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read MCP JSON response");
        assert!(!line.is_empty(), "sandbox-mcp closed stdout");
        serde_json::from_str(&line).expect("parse MCP JSON response")
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct FakeGateway {
    addr: String,
    received: Receiver<Value>,
    worker: Option<JoinHandle<()>>,
}

impl FakeGateway {
    fn response(response: Value) -> Self {
        let mut bytes = serde_json::to_vec(&response).expect("serialize gateway response");
        bytes.push(b'\n');
        Self::raw(bytes)
    }

    fn raw(response: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake gateway");
        let addr = listener.local_addr().expect("fake gateway address");
        let (sender, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept MCP gateway call");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("gateway read timeout");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone gateway stream"))
                .read_line(&mut line)
                .expect("read gateway request");
            let request = serde_json::from_str(&line).expect("parse gateway request");
            sender.send(request).expect("return gateway request");
            stream.write_all(&response).expect("write gateway response");
        });
        Self {
            addr: addr.to_string(),
            received,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> Value {
        let request = self
            .received
            .recv_timeout(Duration::from_secs(5))
            .expect("MCP gateway request");
        self.worker
            .take()
            .expect("fake gateway worker")
            .join()
            .expect("fake gateway worker succeeded");
        request
    }
}

fn result(response: &Value) -> &Value {
    response
        .get("result")
        .unwrap_or_else(|| panic!("expected JSON-RPC result: {response}"))
}

fn structured(response: &Value) -> &Value {
    let result = result(response);
    assert_eq!(result["content"], json!([]), "{response}");
    let value = result
        .get("structuredContent")
        .unwrap_or_else(|| panic!("expected structured tool content: {response}"));
    assert!(
        value.is_object(),
        "structured content is not an object: {response}"
    );
    value
}

fn assert_tool_error<'a>(response: &'a Value, kind: &str) -> &'a Value {
    assert_eq!(result(response)["isError"], true, "{response}");
    let value = structured(response);
    assert_eq!(value["error"]["kind"], kind, "{response}");
    assert!(value["error"]["message"].is_string(), "{response}");
    assert!(value["error"]["details"].is_object(), "{response}");
    value
}

fn set_name(set: OperationSet) -> &'static str {
    match set {
        OperationSet::Management => "management",
        OperationSet::Runtime => "runtime",
        OperationSet::Observability => "observability",
    }
}

fn expected_names(set: OperationSet) -> &'static [&'static str] {
    match set {
        OperationSet::Management => &[
            "create_sandbox",
            "destroy_sandbox",
            "list_sandboxes",
            "inspect_sandbox",
            "squash_layerstacks",
            "export_changes",
        ],
        OperationSet::Runtime => &[
            "exec_command",
            "write_command_stdin",
            "read_command_lines",
            "file_read",
            "file_write",
            "file_edit",
            "file_blame",
        ],
        OperationSet::Observability => &["snapshot", "trace", "events", "cgroup", "layerstack"],
    }
}

fn tools_list_fixture(set: OperationSet) -> Value {
    let fixture = match set {
        OperationSet::Management => include_str!("fixtures/management-tools-list.json"),
        OperationSet::Runtime => include_str!("fixtures/runtime-tools-list.json"),
        OperationSet::Observability => include_str!("fixtures/observability-tools-list.json"),
    };
    serde_json::from_str(fixture).expect("tools/list fixture JSON")
}

fn assert_schema(set: OperationSet, tool: &Value, spec: &CliOperationSpecDocument) {
    assert_eq!(tool["description"], spec.description);
    let schema = tool["inputSchema"].as_object().expect("tool input schema");
    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
    let properties = schema["properties"].as_object().expect("schema properties");

    let mut expected_names = spec
        .args
        .iter()
        .map(|arg| arg.name.as_str())
        .collect::<BTreeSet<_>>();
    if set == OperationSet::Runtime {
        expected_names.insert("sandbox_id");
    }
    assert_eq!(
        properties
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_names,
        "{} schema properties",
        spec.name
    );

    let mut expected_required = spec
        .args
        .iter()
        .filter(|arg| arg.required)
        .map(|arg| arg.name.as_str())
        .collect::<BTreeSet<_>>();
    if set == OperationSet::Runtime {
        expected_required.insert("sandbox_id");
        let selector = &properties["sandbox_id"];
        assert_eq!(selector["type"], "string");
        assert!(selector["description"].is_string());
        assert!(selector.get("default").is_none());
    }
    let required = schema["required"]
        .as_array()
        .expect("schema required")
        .iter()
        .map(|name| name.as_str().expect("required property name"))
        .collect::<BTreeSet<_>>();
    assert_eq!(required, expected_required, "{} required", spec.name);

    for arg in &spec.args {
        let property = &properties[&arg.name];
        assert_eq!(
            property["description"], arg.help,
            "{}.{}",
            spec.name, arg.name
        );
        match arg.kind {
            ArgKind::String | ArgKind::Path => assert_eq!(property["type"], "string"),
            ArgKind::Integer => {
                assert_eq!(property["type"], "integer");
                assert_eq!(property["minimum"], 0);
            }
            ArgKind::Float => assert_eq!(property["type"], "number"),
            ArgKind::JsonArray => assert_eq!(property["type"], "array"),
        }
        match &arg.default {
            Some(default) => assert_eq!(
                property.get("default"),
                Some(&parse_default(arg.kind, default)),
                "{}.{} default",
                spec.name,
                arg.name
            ),
            None => assert!(
                property.get("default").is_none(),
                "{}.{} has an unexpected default",
                spec.name,
                arg.name
            ),
        }
    }

    for hidden in [
        "request_id",
        GATEWAY_AUTH_FIELD,
        "gateway_auth_token",
        "scope",
        "daemon_endpoint",
        "view",
        "export_token",
        "set",
        "lifecycle",
        "file_list",
    ] {
        if hidden != "scope" || spec.name != "cgroup" {
            assert!(
                !properties.contains_key(hidden),
                "{} exposes {hidden}",
                spec.name
            );
            let encoded = serde_json::to_string(schema).expect("serialize tool schema");
            assert!(
                !encoded.contains(&format!("\"{hidden}\":")),
                "{} nests hidden field {hidden}: {encoded}",
                spec.name
            );
        }
    }
}

fn parse_default(kind: ArgKind, default: &str) -> Value {
    match kind {
        ArgKind::String | ArgKind::Path => Value::String(default.to_owned()),
        ArgKind::Integer => Value::from(default.parse::<u64>().expect("integer default")),
        ArgKind::Float => json!(default.parse::<f64>().expect("float default")),
        ArgKind::JsonArray => serde_json::from_str(default).expect("JSON array default"),
    }
}

#[test]
fn lifecycle_and_tools_list_match_all_three_catalogs() {
    for set in [
        OperationSet::Management,
        OperationSet::Runtime,
        OperationSet::Observability,
    ] {
        let mut process = McpProcess::start_initialized(set_name(set), "127.0.0.1:9");
        let response = process.request("tools/list", Some(json!({})));
        assert_eq!(result(&response), &tools_list_fixture(set));
        let tools = result(&response)["tools"].as_array().expect("MCP tools");
        let names = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>();
        assert_eq!(names, expected_names(set), "{} tools", set_name(set));

        let catalog = selected_catalog(set).expect("selected operation catalog");
        assert_eq!(catalog.operations.len(), tools.len());
        for (tool, spec) in tools.iter().zip(&catalog.operations) {
            assert_eq!(tool["name"], spec.name);
            assert_schema(set, tool, spec);
        }

        for hidden in [
            "file_list",
            "create_workspace_session",
            "destroy_workspace_session",
            "get_observability",
            "squash_layerstack",
            "export_layerstack",
            "read_export_chunk",
        ] {
            assert!(
                !names.contains(&hidden),
                "{} exposed {hidden}",
                set_name(set)
            );
        }
    }
}

fn call_through_gateway(
    set: &str,
    tool: &str,
    arguments: Value,
    gateway_response: Value,
) -> (Value, Value) {
    let gateway = FakeGateway::response(gateway_response);
    let mut process = McpProcess::start_initialized(set, &gateway.addr);
    let response = process.call_tool(tool, arguments);
    let request = gateway.finish();
    (response, request)
}

fn assert_common_wire_fields(request: &Value) {
    assert_eq!(request[GATEWAY_AUTH_FIELD], GATEWAY_AUTH_TOKEN);
    assert_eq!(request["_stream_logs"], false);
    let request_id = request["request_id"].as_str().expect("request id");
    assert_eq!(
        request_id.len(),
        36,
        "request id is not a UUID: {request_id}"
    );
    assert_eq!(request_id.matches('-').count(), 4, "{request_id}");
}

#[test]
fn tools_call_routes_every_scope_and_preserves_native_edit_arrays() {
    let success = json!({"fixture": "ok"});
    let (response, request) = call_through_gateway(
        "management",
        "create_sandbox",
        json!({"image": "ubuntu:24.04", "workspace_root": "/workspace"}),
        success.clone(),
    );
    assert_eq!(structured(&response), &success);
    assert_eq!(result(&response)["isError"], false);
    assert_eq!(request["op"], "create_sandbox");
    assert_eq!(request["scope"], json!({"kind": "system"}));
    assert_eq!(
        request["args"],
        json!({"image": "ubuntu:24.04", "workspace_root": "/workspace", "count": 1})
    );
    assert_common_wire_fields(&request);

    let (response, request) = call_through_gateway(
        "runtime",
        "exec_command",
        json!({"sandbox_id": "sbox-runtime", "cmd": "pwd"}),
        success.clone(),
    );
    assert_eq!(structured(&response), &success);
    assert_eq!(request["op"], "exec_command");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "sbox-runtime"})
    );
    assert_eq!(request["args"], json!({"cmd": "pwd"}));
    assert_common_wire_fields(&request);

    let (response, request) =
        call_through_gateway("observability", "snapshot", json!({}), success.clone());
    assert_eq!(structured(&response), &success);
    assert_eq!(request["op"], "snapshot");
    assert_eq!(request["scope"], json!({"kind": "system"}));
    assert_eq!(request["args"], json!({}));
    assert_common_wire_fields(&request);

    let (response, request) = call_through_gateway(
        "observability",
        "trace",
        json!({"sandbox_id": "sbox-observe"}),
        success.clone(),
    );
    assert_eq!(structured(&response), &success);
    assert_eq!(request["op"], "get_observability");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "sbox-observe"})
    );
    assert_eq!(
        request["args"],
        json!({"trace_id": "last", "view": "trace"})
    );
    assert_common_wire_fields(&request);

    let edits = json!([
        {"old_string": "one", "new_string": "two"},
        {"old_string": "x", "new_string": "y", "replace_all": true}
    ]);
    let (response, request) = call_through_gateway(
        "runtime",
        "file_edit",
        json!({"sandbox_id": "sbox-edit", "path": "notes.txt", "edits": edits.clone()}),
        success,
    );
    assert_eq!(structured(&response), &json!({"fixture": "ok"}));
    assert_eq!(request["op"], "file_edit");
    assert_eq!(
        request["scope"],
        json!({"kind": "sandbox", "sandbox_id": "sbox-edit"})
    );
    assert_eq!(
        request["args"],
        json!({"path": "notes.txt", "edits": edits})
    );
    assert!(request["args"]["edits"].is_array());
    assert_common_wire_fields(&request);
}

#[test]
fn invalid_and_hidden_calls_fail_before_gateway_dispatch() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused gateway");
    listener.set_nonblocking(true).expect("nonblocking gateway");
    let addr = listener
        .local_addr()
        .expect("unused gateway address")
        .to_string();
    let mut process = McpProcess::start_initialized("runtime", &addr);

    let cases = [
        (
            "exec_command",
            json!({"cmd": "pwd"}),
            "sandbox_id is required",
        ),
        (
            "exec_command",
            json!({"sandbox_id": 7, "cmd": "pwd"}),
            "sandbox_id must be a string",
        ),
        (
            "exec_command",
            json!({"sandbox_id": "sbox", "cmd": "pwd", "scope": "system"}),
            "unknown argument",
        ),
        (
            "exec_command",
            json!({"sandbox_id": "sbox", "cmd": "pwd", "set": "management"}),
            "unknown argument",
        ),
        (
            "read_command_lines",
            json!({"sandbox_id": "sbox", "command_session_id": "cmd", "limit": "many"}),
            "limit must be an unsigned integer",
        ),
        (
            "file_list",
            json!({"sandbox_id": "sbox"}),
            "unknown operation",
        ),
        (
            "create_workspace_session",
            json!({"sandbox_id": "sbox"}),
            "unknown operation",
        ),
        (
            "destroy_workspace_session",
            json!({"sandbox_id": "sbox"}),
            "unknown operation",
        ),
        (
            "create_sandbox",
            json!({"sandbox_id": "sbox"}),
            "unknown operation",
        ),
        (
            "get_observability",
            json!({"sandbox_id": "sbox"}),
            "unknown operation",
        ),
    ];
    for (tool, arguments, message) in cases {
        let response = process.call_tool(tool, arguments);
        let error = assert_tool_error(&response, "invalid_request");
        assert!(
            error["error"]["message"]
                .as_str()
                .expect("validation message")
                .contains(message),
            "{tool}: {error}"
        );
        assert_eq!(error["error"]["details"], json!({}));
    }

    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "validation unexpectedly dispatched to the gateway"
    );
}

#[test]
fn gateway_operation_errors_keep_kind_message_and_details() {
    let operation_error = json!({
        "error": {
            "kind": "operation_failed",
            "message": "publish failed",
            "details": {"phase": "publish", "retryable": false}
        }
    });
    let (response, request) = call_through_gateway(
        "management",
        "list_sandboxes",
        json!({}),
        operation_error.clone(),
    );
    assert_eq!(
        assert_tool_error(&response, "operation_failed"),
        &operation_error
    );
    assert_eq!(request["op"], "list_sandboxes");
}

#[test]
fn gateway_transport_and_protocol_failures_are_structured_tool_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve closed port");
    let closed_addr = listener
        .local_addr()
        .expect("closed port address")
        .to_string();
    drop(listener);
    let mut process = McpProcess::start_initialized("management", &closed_addr);
    let response = process.call_tool("list_sandboxes", json!({}));
    assert_tool_error(&response, "connection_error");

    for raw in [b"not-json\n".to_vec(), b"[]\n".to_vec()] {
        let gateway = FakeGateway::raw(raw);
        let mut process = McpProcess::start_initialized("management", &gateway.addr);
        let response = process.call_tool("list_sandboxes", json!({}));
        assert_tool_error(&response, "protocol_error");
        let request = gateway.finish();
        assert_eq!(request["op"], "list_sandboxes");
    }
}

#[test]
fn completion_prompt_and_resource_methods_are_not_implemented() {
    let mut process = McpProcess::start_initialized("management", "127.0.0.1:9");
    let unsupported = [
        (
            "completion/complete",
            json!({
                "ref": {"type": "ref/prompt", "name": "fixture"},
                "argument": {"name": "arg", "value": ""}
            }),
        ),
        ("prompts/list", json!({})),
        ("prompts/get", json!({"name": "fixture"})),
        ("resources/list", json!({})),
        ("resources/templates/list", json!({})),
        ("resources/read", json!({"uri": "file:///fixture"})),
        ("resources/subscribe", json!({"uri": "file:///fixture"})),
        ("resources/unsubscribe", json!({"uri": "file:///fixture"})),
    ];
    for (method, params) in unsupported {
        let response = process.request(method, Some(params));
        assert_eq!(response["error"]["code"], -32601, "{method}: {response}");
    }
}

#[test]
fn absent_unknown_and_combined_sets_fail_without_waiting_for_stdin() {
    for args in [
        Vec::<&str>::new(),
        vec!["--set", "all"],
        vec!["--set", "management,runtime"],
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sandbox-mcp"))
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start invalid sandbox-mcp command");
        let _open_stdin = child.stdin.take().expect("keep MCP stdin open");
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll sandbox-mcp") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "sandbox-mcp read stdin for {args:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(2), "unexpected status for {args:?}");

        let mut stdout = String::new();
        child
            .stdout
            .take()
            .expect("invalid command stdout")
            .read_to_string(&mut stdout)
            .expect("read invalid command stdout");
        assert!(stdout.is_empty(), "invalid command wrote stdout: {stdout}");
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("invalid command stderr")
            .read_to_string(&mut stderr)
            .expect("read invalid command stderr");
        assert!(
            stderr.contains("--set"),
            "unexpected error for {args:?}: {stderr}"
        );
    }
}
