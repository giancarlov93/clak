//! The control channel: a named pipe on Windows, a Unix socket on the Mac, line-delimited
//! JSON, reserved to the user. The first line decides the role of whoever connects —
//! `request` is an agent (the command line or the MCP shim), one request per connection;
//! `watch` is the panel.
//!
//! On Windows the channel is also the hub's mutex: if the pipe already exists, there is
//! already a hub. That is why there is always at least one open instance, from startup to
//! shutdown: the new one is born before the previous one passes to whoever serves it. On the
//! Mac the mutex is a `flock` (see [`crate::hub`]).

use crate::core::{Broker, Outbox, Progress};
use crate::requests;
use clak_wire::json::{self, MAX_MESSAGE_BYTES, new_id};
use serde_json::{Map, Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// A freshly accepted connection: the stream and the pid of whoever is on the other side.
pub async fn serve<S>(stream: S, pid: Option<u32>, broker: Broker, stop: CancellationToken)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::with_capacity(64 * 1024, read);
    let (outbox, mut queue) = Outbox::channel();
    let writer = tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            if write.write_all(&json::encode_line(&message)).await.is_err()
                || write.flush().await.is_err()
            {
                return;
            }
        }
    });

    if let Some(first) = next(&mut lines, &stop).await {
        match first.get("type").and_then(Value::as_str) {
            Some("watch") => watch(&broker, lines, &outbox, &stop).await,
            Some("request") => request(&broker, first, lines, &outbox, pid, &stop).await,
            _ => {}
        }
    }
    drop(outbox);
    // Waits for the queue to be written, but not forever: a peer that does not read keeps nothing open.
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
}

/// A request from an agent. The operating system says who made it: the process at the
/// other end of the channel and, walking up, the agent that launched it. If the requester
/// closes the connection while waiting, the question to the user is withdrawn (unless someone
/// else is waiting for the same answer).
async fn request<R>(
    broker: &Broker,
    request: Map<String, Value>,
    mut lines: BufReader<R>,
    outbox: &Outbox,
    pid: Option<u32>,
    stop: &CancellationToken,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let gone = stop.child_token();
    // Nothing else arrives after the request: a read that ends means the requester has closed.
    let watcher = {
        let gone = gone.clone();
        tokio::spawn(async move {
            let mut sink = Vec::new();
            loop {
                sink.clear();
                tokio::select! {
                    read = lines.read_until(b'\n', &mut sink) => {
                        if !matches!(read, Ok(n) if n > 0) {
                            break;
                        }
                    }
                    _ = gone.cancelled() => return,
                }
            }
            gone.cancel();
        })
    };

    let platform = broker.options().platform.clone();
    let keys = broker.run(|c| c.session_keys());
    let asked = request.clone();
    let agent = tokio::task::spawn_blocking(move || platform.agent_of(pid, &asked, &keys))
        .await
        .ok()
        .flatten();
    if let Ok(mut response) = requests::handle(
        broker,
        agent,
        &request,
        &gone,
        Progress::new(Some(outbox.clone())),
    )
    .await
    {
        response.insert("type".into(), Value::String("response".into()));
        outbox.send(Value::Object(response));
    }
    gone.cancel();
    let _ = watcher.await;
}

/// The panel. It sees everything and can only *take away*: revoke a permission, disconnect
/// an agent, forget a program. Never grant one: permissions are granted only in the
/// questions the hub shows itself.
async fn watch<R>(
    broker: &Broker,
    mut lines: BufReader<R>,
    outbox: &Outbox,
    stop: &CancellationToken,
) where
    R: AsyncRead + Unpin,
{
    let monitor = new_id("mon");
    broker.run(|c| c.watch(&monitor, outbox.clone()));
    while let Some(message) = next(&mut lines, stop).await {
        panel_message(broker, &message, outbox, false);
    }
    broker.run(|c| c.unwatch(&monitor));
}

/// What the panel asks: the same from the channel and from the app hosting the hub.
/// `inside` is that app: only it gets a call in full (`call`), with what the agent sent and
/// what came back. On the channel any process of the user could ask, an agent included.
pub fn panel_message(broker: &Broker, message: &Map<String, Value>, reply: &Outbox, inside: bool) {
    let text = |key: &str| message.get(key).and_then(Value::as_str);
    broker.run(|c| {
        match text("type") {
            Some("revoke") => c.revoke(text("grant_id")),
            Some("forget_session") => c.forget_session(text("session_id"), "user"),
            Some("forget_app") => c.forget_app(text("key")),
            Some("forget_rule") => c.forget_rule(text("id")),
            Some("describe") => {
                reply.send(c.panel_tools(text("resource_id")));
                return;
            }
            Some("history") => {
                reply.send(c.panel_history(text("grant_id")));
                return;
            }
            Some("usage") => {
                reply.send(c.panel_usage());
                return;
            }
            Some("call") => {
                reply.send(if inside {
                    c.panel_call(text("grant_id"), text("call_id"))
                } else {
                    json!({ "type": "call", "grant_id": text("grant_id"), "call_id": text("call_id"), "found": false, "remote": true })
                });
                return;
            }
            _ => {}
        }
        c.touch();
    });
}

/// The next message, or nothing if the channel is closed. An empty or non-JSON line is
/// skipped; one that is JSON but not an object closes the channel, because whoever sends it
/// is not speaking this protocol.
async fn next<R>(lines: &mut BufReader<R>, stop: &CancellationToken) -> Option<Map<String, Value>>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = tokio::select! {
            read = read_line(lines, &mut line) => read,
            _ = stop.cancelled() => return None,
        };
        match read {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        let text = line.trim_ascii();
        if text.is_empty() {
            continue;
        }
        match json::parse(text) {
            Ok(Value::Object(message)) => return Some(message),
            Ok(_) => return None,
            Err(_) => continue,
        }
    }
}

/// One line, with a cap: beyond [`MAX_MESSAGE_BYTES`] it is not a message.
async fn read_line<R>(lines: &mut BufReader<R>, into: &mut Vec<u8>) -> std::io::Result<usize>
where
    R: AsyncRead + Unpin,
{
    let mut total = 0;
    loop {
        let available = lines.fill_buf().await?;
        if available.is_empty() {
            return Ok(total);
        }
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..=i], true),
            None => (available, false),
        };
        into.extend_from_slice(chunk);
        let used = chunk.len();
        lines.consume(used);
        total += used;
        if into.len() > MAX_MESSAGE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
        if done {
            return Ok(total);
        }
    }
}

// =========================================================================
// Windows: the named pipe
// =========================================================================

#[cfg(windows)]
pub mod pipe {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Several listening instances: whoever arrives while another connection is being taken
    /// on finds room at once, instead of a «pipe busy» to retry.
    const LISTENERS: usize = 4;

    /// A security descriptor that lets in only the user who started the hub.
    struct UserOnly {
        descriptor: PSECURITY_DESCRIPTOR,
    }

    unsafe impl Send for UserOnly {}
    unsafe impl Sync for UserOnly {}

    impl Drop for UserOnly {
        fn drop(&mut self) {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe { LocalFree(self.descriptor as _) };
        }
    }

    impl UserOnly {
        fn new() -> std::io::Result<UserOnly> {
            let sid = current_user_sid()?;
            // Only the user, full access; protected, i.e. without inherited permissions.
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            // SAFETY: null-terminated text, valid output pointer.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(UserOnly { descriptor })
        }

        fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor,
                bInheritHandle: 0,
            }
        }
    }

    fn current_user_sid() -> std::io::Result<String> {
        // SAFETY: Win32 calls with buffers of the size they ask for.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut size = 0u32;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
            let mut buffer = vec![0u8; size as usize];
            let ok = GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            );
            CloseHandle(token);
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let mut text: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text as _);
            Ok(sid)
        }
    }

    /// One instance of the pipe. With `first` creation succeeds only if the pipe does not
    /// exist yet: that is where it is decided, atomically, who the hub is.
    pub fn open(name: &str, first: bool) -> std::io::Result<NamedPipeServer> {
        let security = UserOnly::new()?;
        let mut attributes = security.attributes();
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(254);
        // SAFETY: the attributes stay valid for the whole call.
        unsafe {
            options.create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            )
        }
    }

    pub fn client_pid(pipe: &NamedPipeServer) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: the handle is alive as long as the pipe is.
        let ok = unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) };
        (ok != 0 && pid != 0).then_some(pid)
    }

    pub async fn listen(
        first: NamedPipeServer,
        name: String,
        broker: Broker,
        tracker: TaskTracker,
        stop: CancellationToken,
    ) {
        let mut loops = Vec::new();
        let mut first = Some(first);
        for _ in 0..LISTENERS {
            loops.push(tokio::spawn(listen_one(
                first.take(),
                name.clone(),
                broker.clone(),
                tracker.clone(),
                stop.clone(),
            )));
        }
        for task in loops {
            let _ = task.await;
        }
    }

    async fn reopen(name: &str, stop: &CancellationToken) -> Option<NamedPipeServer> {
        loop {
            if stop.is_cancelled() {
                return None;
            }
            match open(name, false) {
                Ok(pipe) => return Some(pipe),
                Err(_) => {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                        _ = stop.cancelled() => return None,
                    }
                }
            }
        }
    }

    async fn listen_one(
        mut listening: Option<NamedPipeServer>,
        name: String,
        broker: Broker,
        tracker: TaskTracker,
        stop: CancellationToken,
    ) {
        loop {
            let pipe = match listening.take() {
                Some(pipe) => pipe,
                None => match reopen(&name, &stop).await {
                    Some(pipe) => pipe,
                    None => return,
                },
            };
            let connected = tokio::select! {
                connected = pipe.connect() => connected,
                _ = stop.cancelled() => return,
            };
            // Whoever connected left before being served: the instance must be thrown away,
            // but only after another one exists.
            listening = reopen(&name, &stop).await;
            if listening.is_none() {
                return;
            }
            if connected.is_err() {
                continue;
            }
            let pid = client_pid(&pipe);
            tracker.spawn(serve(pipe, pid, broker.clone(), stop.clone()));
        }
    }
}

// =========================================================================
// Mac (and Unix): the socket
// =========================================================================

#[cfg(unix)]
pub mod socket {
    use super::*;
    use tokio::net::UnixListener;

    pub async fn listen(
        listener: UnixListener,
        broker: Broker,
        tracker: TaskTracker,
        stop: CancellationToken,
    ) {
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = stop.cancelled() => return,
            };
            let Ok((stream, _)) = accepted else { continue };
            let pid = stream
                .peer_cred()
                .ok()
                .and_then(|c| c.pid())
                .and_then(|p| u32::try_from(p).ok())
                .filter(|p| *p > 0);
            tracker.spawn(serve(stream, pid, broker.clone(), stop.clone()));
        }
    }
}
