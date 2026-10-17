//! An MCP client over the stdio transport: the server is a program the hub starts, and
//! JSON-RPC messages travel one per line on its stdin and stdout. Stderr is its log:
//! its last lines are kept, to tell why it stopped.
//!
//! The program lives as long as the client. When the last reference goes away stdin is
//! closed, and an MCP server then exits by itself; if it's still there after a few
//! seconds, it's stopped. In any case, once the program is over, what it launched stops
//! too (`npx` launches `node`): on Windows with a job object, which the system closes even
//! if the hub dies, on other systems with the process group.
//!
//! Canceling a request means dropping its future: the server learns about it with
//! `notifications/cancelled`.

use crate::launch::{self, Launch};
use crate::mcp_http::{Elicit, McpError, PROTOCOL_VERSION};
use clak_wire::json::{self as wire, MAX_MESSAGE_BYTES};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// How many stderr lines are kept, and how long.
const STDERR_LINES: usize = 20;
const STDERR_WIDTH: usize = 500;

/// How long to wait for the program to exit by itself, after closing its stdin.
const GRACE: Duration = Duration::from_secs(3);

type Answer = oneshot::Sender<Result<Map<String, Value>, McpError>>;

#[derive(Default)]
struct State {
    instructions: Option<String>,
    server_name: Option<String>,
    /// Why the program is gone, when it's gone.
    ended: Option<String>,
    stderr: VecDeque<String>,
    pending: HashMap<i64, Answer>,
    /// The confirmations of the running commands: a request from the server goes to the most recent.
    elicits: Vec<(i64, Elicit)>,
}

struct Shared {
    state: Mutex<State>,
    list_changed: Notify,
    ended: CancellationToken,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The program is gone: whoever was waiting for an answer knows, with the reason.
    fn end(&self, reason: String) {
        let pending: Vec<Answer> = {
            let mut state = self.lock();
            if state.ended.is_none() {
                state.ended = Some(reason.clone());
            }
            state.pending.drain().map(|(_, answer)| answer).collect()
        };
        let reason = self.lock().ended.clone().unwrap_or(reason);
        for answer in pending {
            let _ = answer.send(Err(McpError::Transport(reason.clone())));
        }
        self.ended.cancel();
    }

    /// The last line the program wrote on stderr, if there is one.
    fn last_words(&self) -> Option<String> {
        self.lock().stderr.back().cloned()
    }
}

/// The MCP client of a program started by the hub. Used by several requests at once.
pub struct McpStdio {
    out: mpsc::UnboundedSender<String>,
    shared: Arc<Shared>,
    next_id: AtomicI64,
    can_elicit: bool,
    stop: CancellationToken,
}

impl Drop for McpStdio {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl McpStdio {
    /// Starts the program, with the extra environment variables `env`. Doesn't talk yet:
    /// [`McpStdio::initialize`] does.
    pub async fn start(
        launch: &Launch,
        env: &BTreeMap<String, String>,
        can_elicit: bool,
    ) -> Result<McpStdio, McpError> {
        let path = launch::user_path_async().await;
        let program = launch::resolve(&launch.program, &path).ok_or_else(|| {
            McpError::Transport(format!(
                "“{}” was not found: neither a file nor a program in the PATH",
                launch.program
            ))
        })?;
        let mut command = Command::new(&program);
        command
            .args(&launch.args)
            .env("PATH", &path)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(home) = dirs_home() {
            command.current_dir(home);
        }
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .map_err(|e| McpError::Transport(format!("“{}” didn't start: {e}", launch.program)))?;
        let tree = Tree::of(&child);
        let (stdin, stdout, stderr) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (Some(mut stdin), Some(stdout), Some(stderr)) = (stdin, stdout, stderr) else {
            return Err(McpError::Transport(
                "the program's input and output are not available".into(),
            ));
        };

        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            list_changed: Notify::new(),
            ended: CancellationToken::new(),
        });
        let stop = CancellationToken::new();
        let (out, mut queue) = mpsc::unbounded_channel::<String>();

        // There is only one writer: one message per line, in the order they were sent. When
        // the client goes away stdin closes (the queue doesn't: whoever reads stdout holds it too).
        let closing = stop.clone();
        tokio::spawn(async move {
            while let Some(line) = tokio::select! {
                line = queue.recv() => line,
                _ = closing.cancelled() => None,
            } {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });
        tokio::spawn(read_stderr(BufReader::new(stderr), shared.clone()));
        tokio::spawn(read_stdout(
            BufReader::new(stdout),
            shared.clone(),
            out.clone(),
            can_elicit,
        ));
        tokio::spawn(watch(child, tree, shared.clone(), stop.clone()));

        Ok(McpStdio {
            out,
            shared,
            next_id: AtomicI64::new(0),
            can_elicit,
            stop,
        })
    }

    /// The instructions for how the server wants to be used, if it has any.
    pub fn instructions(&self) -> Option<String> {
        self.shared.lock().instructions.clone()
    }

    pub fn server_name(&self) -> Option<String> {
        self.shared.lock().server_name.clone()
    }

    /// Arrives when the server says its commands have changed.
    pub async fn changed(&self) {
        self.shared.list_changed.notified().await;
    }

    /// Arrives when the program is gone; says why.
    pub async fn ended(&self) -> String {
        self.shared.ended.cancelled().await;
        self.shared
            .lock()
            .ended
            .clone()
            .unwrap_or_else(|| "the program stopped".into())
    }

    pub async fn initialize(&self) -> Result<(), McpError> {
        let mut capabilities = Map::new();
        if self.can_elicit {
            capabilities.insert("elicitation".into(), json!({}));
        }
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": capabilities,
                    "clientInfo": { "name": "clak", "version": env!("CARGO_PKG_VERSION") },
                }),
                None,
            )
            .await?;
        {
            let mut state = self.shared.lock();
            state.instructions = wire::str(result.get("instructions"))
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            state.server_name =
                wire::str(result.get("serverInfo").and_then(|s| s.get("name"))).map(str::to_owned);
        }
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
    }

    /// All the commands, page after page.
    pub async fn list_tools(&self) -> Result<Vec<Map<String, Value>>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let mut parameters = Map::new();
            if let Some(cursor) = &cursor {
                parameters.insert("cursor".into(), Value::String(cursor.clone()));
            }
            let result = self
                .request("tools/list", Value::Object(parameters), None)
                .await?;
            if let Some(Value::Array(list)) = result.get("tools") {
                tools.extend(list.iter().filter_map(|t| t.as_object().cloned()));
            }
            cursor = wire::str(result.get("nextCursor")).map(str::to_owned);
            if cursor.as_deref().is_none_or(str::is_empty) {
                break;
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
        elicit: Option<&Elicit>,
    ) -> Result<Map<String, Value>, McpError> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
            elicit,
        )
        .await
    }

    fn send(&self, message: &Value) -> Result<(), McpError> {
        self.out
            .send(wire::text(message))
            .map_err(|_| McpError::Transport(self.gone()))
    }

    fn gone(&self) -> String {
        self.shared
            .lock()
            .ended
            .clone()
            .unwrap_or_else(|| "the program stopped".into())
    }

    async fn request(
        &self,
        method: &str,
        parameters: Value,
        elicit: Option<&Elicit>,
    ) -> Result<Map<String, Value>, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (answer, receiver) = oneshot::channel();
        {
            let mut state = self.shared.lock();
            if let Some(reason) = &state.ended {
                return Err(McpError::Transport(reason.clone()));
            }
            state.pending.insert(id, answer);
            if let Some(elicit) = elicit {
                state.elicits.push((id, elicit.clone()));
            }
        }
        let mut waiting = Waiting {
            client: self,
            id,
            method_cancels: method != "initialize",
            done: false,
        };
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": parameters }))?;
        let result = receiver
            .await
            .unwrap_or_else(|_| Err(McpError::Transport(self.gone())));
        waiting.done = true;
        result
    }
}

/// A request waiting for an answer: if whoever was waiting leaves first, the server knows.
struct Waiting<'a> {
    client: &'a McpStdio,
    id: i64,
    method_cancels: bool,
    done: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let abandoned = {
            let mut state = self.client.shared.lock();
            state.elicits.retain(|(id, _)| *id != self.id);
            state.pending.remove(&self.id).is_some()
        };
        if abandoned && !self.done && self.method_cancels {
            let _ = self.client.send(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": { "requestId": self.id, "reason": "the caller stopped waiting" },
            }));
        }
    }
}

/// The server's messages, one per line: responses, its requests, notifications. A line
/// that isn't JSON (a server that writes its log on stdout) is skipped.
async fn read_stdout<R: AsyncBufRead + Unpin>(
    mut reader: R,
    shared: Arc<Shared>,
    out: mpsc::UnboundedSender<String>,
    can_elicit: bool,
) {
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_line(&mut reader, &mut line).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                shared.end(format!("the program sent an unreadable message ({e})"));
                return;
            }
        }
        let Ok(Value::Object(message)) = wire::parse(&line) else {
            continue;
        };
        let method = wire::str(message.get("method")).map(str::to_owned);
        let id = message.get("id").filter(|v| !v.is_null()).cloned();
        match (method, id) {
            (None, Some(id)) => answer(&shared, &message, &id),
            (Some(method), Some(id)) => {
                let parameters = message
                    .get("params")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let elicit = shared.lock().elicits.last().map(|(_, e)| e.clone());
                let out = out.clone();
                // In a task: a confirmation waits for the user, and meanwhile the answers arrive.
                tokio::spawn(async move {
                    let reply = reply(id, &method, parameters, elicit.filter(|_| can_elicit)).await;
                    let _ = out.send(wire::text(&reply));
                });
            }
            (Some(method), None) if method == "notifications/tools/list_changed" => {
                shared.list_changed.notify_one()
            }
            _ => {}
        }
    }
    // stdout closed: the program is ending, or has ended. Whoever watches it tells why.
    tokio::select! {
        _ = shared.ended.cancelled() => {}
        _ = tokio::time::sleep(GRACE) => shared.end("the program closed its output".into()),
    }
}

fn answer(shared: &Shared, message: &Map<String, Value>, id: &Value) {
    let Some(id) = id.as_i64() else { return };
    let Some(answer) = shared.lock().pending.remove(&id) else {
        return;
    };
    let result = match message.get("error") {
        Some(Value::Object(error)) => Err(McpError::Rpc {
            code: error
                .get("code")
                .and_then(Value::as_i64)
                .and_then(|c| i32::try_from(c).ok())
                .unwrap_or(0),
            message: wire::str(error.get("message"))
                .unwrap_or("server error")
                .to_owned(),
        }),
        _ => Ok(message
            .get("result")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()),
    };
    let _ = answer.send(result);
}

/// The answer to a request from the server: `ping`, and the confirmation
/// (`elicitation/create`) if there is a running command that can ask the user for it.
async fn reply(
    id: Value,
    method: &str,
    parameters: Map<String, Value>,
    elicit: Option<Elicit>,
) -> Value {
    match (method, elicit) {
        ("ping", _) => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        ("elicitation/create", Some(elicit)) => {
            json!({ "jsonrpc": "2.0", "id": id, "result": Value::Object(elicit(parameters).await) })
        }
        ("elicitation/create", None) => {
            json!({ "jsonrpc": "2.0", "id": id, "result": { "action": "cancel" } })
        }
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("the client doesn't support '{method}'") },
        }),
    }
}

/// A line, without its newline; `false` at the end. Never longer than a message.
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<bool> {
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(!line.is_empty());
        }
        match buffer.iter().position(|&b| b == b'\n') {
            Some(at) => {
                line.extend_from_slice(&buffer[..at]);
                reader.consume(at + 1);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(true);
            }
            None => {
                let n = buffer.len();
                line.extend_from_slice(buffer);
                reader.consume(n);
            }
        }
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(std::io::Error::other("message too large"));
        }
    }
}

async fn read_stderr<R: AsyncBufRead + Unpin>(mut reader: R, shared: Arc<Shared>) {
    let mut line = Vec::new();
    loop {
        line.clear();
        // A huge line is noise: keep its beginning.
        match read_line(&mut reader, &mut line).await {
            Ok(true) => {}
            Ok(false) | Err(_) => return,
        }
        let text: String = String::from_utf8_lossy(&line)
            .trim()
            .chars()
            .take(STDERR_WIDTH)
            .collect();
        if text.is_empty() {
            continue;
        }
        let mut state = shared.lock();
        if state.stderr.len() == STDERR_LINES {
            state.stderr.pop_front();
        }
        state.stderr.push_back(text);
    }
}

/// Waits for the program to end: by itself, or because the client went away (stdin closed,
/// then, if needed, it's stopped). Then what the program had launched is stopped.
async fn watch(mut child: Child, tree: Tree, shared: Arc<Shared>, stop: CancellationToken) {
    let status = tokio::select! {
        status = child.wait() => Some(status),
        _ = stop.cancelled() => None,
    };
    let status = match status {
        Some(status) => status,
        None => match tokio::time::timeout(GRACE, child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                tree.kill();
                let _ = child.start_kill();
                child.wait().await
            }
        },
    };
    tree.kill();
    // Stderr finishes arriving a moment after the exit.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let how = match status {
        Ok(s) if s.success() => "the program exited".to_owned(),
        Ok(s) => match s.code() {
            Some(code) => format!("the program exited with code {code}"),
            None => "the program was stopped".to_owned(),
        },
        Err(e) => format!("the program can't be followed ({e})"),
    };
    let reason = match shared.last_words() {
        Some(words) => format!("{how}: {words}"),
        None => how,
    };
    shared.end(reason);
}

fn dirs_home() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(std::path::PathBuf::from).filter(|p| p.is_dir())
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The program and what it launches, to stop all together.
#[cfg(windows)]
struct Tree(Option<windows_sys::Win32::Foundation::HANDLE>);

// A job handle can be used from any thread.
#[cfg(windows)]
unsafe impl Send for Tree {}

#[cfg(windows)]
impl Tree {
    /// A job object that stops its processes when closed: even if the hub dies,
    /// because then the system closes the handle.
    fn of(child: &Child) -> Tree {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        let Some(process) = child.raw_handle() else {
            return Tree(None);
        };
        // SAFETY: Win32 calls with valid handles and a structure of the declared size.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Tree(None);
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 || AssignProcessToJobObject(job, process as _) == 0 {
                CloseHandle(job);
                return Tree(None);
            }
            Tree(Some(job))
        }
    }

    fn kill(&self) {
        if let Some(job) = self.0 {
            // SAFETY: the job is ours and open.
            unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(job, 1) };
        }
    }
}

#[cfg(windows)]
impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            // SAFETY: closed only once.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
        }
    }
}

/// The program and what it launches: its process group.
#[cfg(unix)]
struct Tree(Option<i32>);

#[cfg(unix)]
impl Tree {
    fn of(child: &Child) -> Tree {
        Tree(child.id().and_then(|pid| i32::try_from(pid).ok()))
    }

    fn kill(&self) {
        if let Some(group) = self.0 {
            // SAFETY: a signal to the group the program created when starting; if it's gone, nothing.
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn le_righe_si_leggono_intere_e_senza_a_capo() {
        let data: &[u8] = b"{\"a\":1}\r\n\nultima";
        let mut reader = BufReader::new(data);
        let mut line = Vec::new();
        assert!(read_line(&mut reader, &mut line).await.unwrap());
        assert_eq!(line, b"{\"a\":1}");
        line.clear();
        assert!(read_line(&mut reader, &mut line).await.unwrap());
        assert!(line.is_empty());
        line.clear();
        assert!(read_line(&mut reader, &mut line).await.unwrap());
        assert_eq!(line, b"ultima");
        line.clear();
        assert!(!read_line(&mut reader, &mut line).await.unwrap());
    }

    #[test]
    fn un_programma_che_non_c_e_lo_dice() {
        let launch = Launch::parse("clak-programma-che-non-esiste --x").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime.block_on(async {
            McpStdio::start(&launch, &BTreeMap::new(), false)
                .await
                .err()
        });
        assert!(error.unwrap().to_string().contains("was not found"));
    }
}
