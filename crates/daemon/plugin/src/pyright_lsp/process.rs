use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use layerstack::LayerPath;
use serde_json::{json, Value};

use crate::PluginRuntimeError;

use super::lsp_values::{
    diagnostic_value, file_uri, flatten_symbols, locations_from_lsp_result, target_file_uri,
    target_file_uri_string,
};
use super::LANGUAGE_ID;

pub(super) struct LspProcess {
    child: Child,
    process_group_id: Option<i32>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<BTreeMap<String, Sender<Result<Value, String>>>>>,
    diagnostics: Arc<DiagnosticsState>,
    open_documents: BTreeMap<String, i64>,
    next_id: AtomicU64,
    reader: Option<JoinHandle<()>>,
    torn_down: bool,
}

impl LspProcess {
    pub(super) fn start(
        command: Vec<String>,
        workspace_root: &Path,
        max_response_bytes: usize,
    ) -> Result<Self, String> {
        let Some((program, args)) = command.split_first() else {
            return Err("pyright_lsp command is empty".to_owned());
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|err| err.to_string())?;
        let process_group_id = i32::try_from(child.id()).ok();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open pyright_lsp stdin".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to open pyright_lsp stdout".to_owned())?;
        let stdin = Arc::new(Mutex::new(stdin));
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        let diagnostics = Arc::new(DiagnosticsState::default());
        let reader = spawn_reader(
            stdout,
            Arc::clone(&stdin),
            Arc::clone(&pending),
            Arc::clone(&diagnostics),
            max_response_bytes,
        );
        Ok(Self {
            child,
            process_group_id,
            stdin,
            pending,
            diagnostics,
            open_documents: BTreeMap::new(),
            next_id: AtomicU64::new(1),
            reader: Some(reader),
            torn_down: false,
        })
    }

    pub(super) fn initialize(
        &mut self,
        workspace_root: &Path,
        timeout: Duration,
    ) -> Result<Value, String> {
        let root_uri = file_uri(workspace_root);
        let result = self.request(
            "initialize",
            json!({
                "processId": Value::Null,
                "rootUri": root_uri,
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": "pyright_lsp",
                }],
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": {
                            "relatedInformation": true,
                            "versionSupport": true,
                        }
                    },
                    "workspace": {
                        "configuration": true,
                        "didChangeWatchedFiles": {
                            "dynamicRegistration": false,
                        }
                    }
                }
            }),
            timeout,
        )?;
        self.notify("initialized", json!({}))?;
        Ok(result)
    }

    pub(super) fn document_symbols(
        &mut self,
        projection_root: &Path,
        file_path: &str,
        query: Option<&str>,
    ) -> Result<Vec<Value>, PluginRuntimeError> {
        let uri = target_file_uri(projection_root, file_path)?;
        let result = self
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": uri } }),
                Duration::from_secs(30),
            )
            .map_err(PluginRuntimeError::PyrightLsp)?;
        let mut symbols = Vec::new();
        let query = query.map(str::to_ascii_lowercase);
        flatten_symbols(
            &result,
            projection_root,
            file_path,
            query.as_deref(),
            &mut symbols,
        );
        Ok(symbols)
    }

    pub(super) fn definition(
        &mut self,
        projection_root: &Path,
        file_path: &str,
        line: u64,
        character: u64,
    ) -> Result<Vec<Value>, PluginRuntimeError> {
        let uri = target_file_uri(projection_root, file_path)?;
        let result = self
            .request(
                "textDocument/definition",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                }),
                Duration::from_secs(30),
            )
            .map_err(PluginRuntimeError::PyrightLsp)?;
        Ok(locations_from_lsp_result(&result, projection_root))
    }

    pub(super) fn references(
        &mut self,
        projection_root: &Path,
        file_path: &str,
        line: u64,
        character: u64,
        include_declaration: bool,
    ) -> Result<Vec<Value>, PluginRuntimeError> {
        let uri = target_file_uri(projection_root, file_path)?;
        let result = self
            .request(
                "textDocument/references",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                    "context": { "includeDeclaration": include_declaration },
                }),
                Duration::from_secs(30),
            )
            .map_err(PluginRuntimeError::PyrightLsp)?;
        Ok(locations_from_lsp_result(&result, projection_root))
    }

    pub(super) fn diagnostics(
        &mut self,
        projection_root: &Path,
        file_path: &str,
        timeout: Duration,
    ) -> Result<Vec<Value>, String> {
        let uri =
            target_file_uri_string(projection_root, file_path).map_err(|err| err.to_string())?;
        self.diagnostics.wait_for_uri(&uri, timeout).map(|items| {
            items
                .into_iter()
                .map(|diagnostic| diagnostic_value(&uri, projection_root, diagnostic))
                .collect()
        })
    }

    pub(super) fn open_document(
        &mut self,
        projection_root: &Path,
        file_path: &str,
        manifest_version: i64,
    ) -> Result<(), PluginRuntimeError> {
        let normalized = LayerPath::parse(file_path).map_err(|err| {
            PluginRuntimeError::InvalidRequest(format!("invalid file_path for pyright_lsp: {err}"))
        })?;
        let path = projection_root.join(normalized.as_str());
        let text = std::fs::read_to_string(&path).map_err(|err| {
            PluginRuntimeError::InvalidRequest(format!(
                "pyright_lsp target file is not readable: {}: {err}",
                normalized.as_str()
            ))
        })?;
        let uri = file_uri(&path);
        let version = manifest_version.max(0);
        if self.open_documents.contains_key(&uri) {
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "version": version,
                    },
                    "contentChanges": [{ "text": text }],
                }),
            )
            .map_err(PluginRuntimeError::PyrightLsp)?;
        } else {
            self.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": LANGUAGE_ID,
                        "version": version,
                        "text": text,
                    }
                }),
            )
            .map_err(PluginRuntimeError::PyrightLsp)?;
        }
        self.open_documents.insert(uri, version);
        Ok(())
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let key = id.to_string();
        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| "pyright_lsp pending map lock poisoned".to_owned())?
            .insert(key.clone(), tx);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(err) = self.write_message(&message) {
            let _ = self.pending.lock().map(|mut pending| pending.remove(&key));
            return Err(err);
        }
        match rx.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(err),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.pending.lock().map(|mut pending| pending.remove(&key));
                Err(format!("pyright_lsp request {method} timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(format!("pyright_lsp request {method} disconnected"))
            }
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn write_message(&self, message: &Value) -> Result<(), String> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| "pyright_lsp stdin lock poisoned".to_owned())?;
        write_lsp_message(&mut *stdin, message).map_err(|err| err.to_string())
    }

    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn is_running(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    pub(super) fn teardown(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;
        terminate_process_group(self.process_group_id);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[derive(Default)]
struct DiagnosticsState {
    entries: Mutex<BTreeMap<String, Vec<Value>>>,
    changed: Condvar,
}

impl DiagnosticsState {
    fn record(&self, uri: String, diagnostics: Vec<Value>) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(uri, diagnostics);
            self.changed.notify_all();
        }
    }

    fn wait_for_uri(&self, uri: &str, timeout: Duration) -> Result<Vec<Value>, String> {
        let deadline = Instant::now() + timeout;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "pyright_lsp diagnostics lock poisoned".to_owned())?;
        loop {
            if let Some(diagnostics) = entries.get(uri) {
                return Ok(diagnostics.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!("timed out waiting for diagnostics for {uri}"));
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next_entries, timeout) = self
                .changed
                .wait_timeout(entries, remaining)
                .map_err(|_| "pyright_lsp diagnostics lock poisoned".to_owned())?;
            entries = next_entries;
            if timeout.timed_out() {
                return Err(format!("timed out waiting for diagnostics for {uri}"));
            }
        }
    }
}

fn spawn_reader(
    stdout: std::process::ChildStdout,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<BTreeMap<String, Sender<Result<Value, String>>>>>,
    diagnostics: Arc<DiagnosticsState>,
    max_response_bytes: usize,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let message = match read_lsp_message(&mut reader, max_response_bytes) {
                Ok(Some(message)) => message,
                Ok(None) => return,
                Err(err) => {
                    fail_pending(&pending, err);
                    return;
                }
            };
            handle_lsp_message(&message, &stdin, &pending, &diagnostics);
        }
    })
}

fn handle_lsp_message(
    message: &Value,
    stdin: &Arc<Mutex<ChildStdin>>,
    pending: &Arc<Mutex<BTreeMap<String, Sender<Result<Value, String>>>>>,
    diagnostics: &DiagnosticsState,
) {
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        if method == "textDocument/publishDiagnostics" {
            if let Some(params) = message.get("params") {
                let uri = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let items = params
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if !uri.is_empty() {
                    diagnostics.record(uri, items);
                }
            }
        }
        if message.get("id").is_some() {
            respond_to_server_request(message, stdin);
        }
        return;
    }

    let Some(id) = message.get("id").map(lsp_id_key) else {
        return;
    };
    let sender = pending
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(&id));
    if let Some(sender) = sender {
        let _ = if let Some(error) = message.get("error") {
            sender.send(Err(error.to_string()))
        } else {
            sender.send(Ok(message.get("result").cloned().unwrap_or(Value::Null)))
        };
    }
}

fn respond_to_server_request(message: &Value, stdin: &Arc<Mutex<ChildStdin>>) {
    let Some(id) = message.get("id").cloned() else {
        return;
    };
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "workspace/configuration" => {
            let count = message
                .get("params")
                .and_then(|params| params.get("items"))
                .and_then(Value::as_array)
                .map_or(1, Vec::len);
            Value::Array((0..count).map(|_| json!({})).collect())
        }
        _ => Value::Null,
    };
    if let Ok(mut stdin) = stdin.lock() {
        let _ = write_lsp_message(
            &mut *stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            }),
        );
    }
}

fn fail_pending(
    pending: &Arc<Mutex<BTreeMap<String, Sender<Result<Value, String>>>>>,
    error: String,
) {
    if let Ok(mut pending) = pending.lock() {
        let senders = std::mem::take(&mut *pending);
        for sender in senders.into_values() {
            let _ = sender.send(Err(error.clone()));
        }
    }
}

fn read_lsp_message(
    reader: &mut BufReader<std::process::ChildStdout>,
    max_response_bytes: usize,
) -> Result<Option<Value>, String> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|err| err.to_string())?;
        if read == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|err| format!("invalid LSP Content-Length: {err}"))?;
            content_length = Some(length);
        }
    }
    let content_length = content_length.ok_or_else(|| "missing LSP Content-Length".to_owned())?;
    if content_length > max_response_bytes {
        return Err(format!(
            "pyright_lsp response exceeds {} byte limit",
            max_response_bytes
        ));
    }
    let mut body = vec![0_u8; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|err| err.to_string())?;
    serde_json::from_slice(&body).map_err(|err| err.to_string())
}

fn write_lsp_message(writer: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

fn lsp_id_key(id: &Value) -> String {
    match id {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => id.to_string(),
    }
}

fn terminate_process_group(process_group_id: Option<i32>) {
    let Some(pgid) = process_group_id else {
        return;
    };
    let pid = nix::unistd::Pid::from_raw(pgid);
    if nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGTERM).is_ok() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
}
