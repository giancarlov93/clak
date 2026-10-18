//! A fake hub on the control channel, in its own folder: to every request it replies with
//! the lines the test wrote (waiting lines, a response, or nothing), then keeps the
//! connection open until the client closes it. This tests the agent side without
//! depending on what the real hub says today.

use crate::util::Object;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) struct FakeHub {
    pub home: PathBuf,
    state: Arc<State>,
}

struct State {
    script: Vec<Value>,
    received: Mutex<Vec<Object>>,
    closed: AtomicUsize,
    stop: AtomicBool,
}

impl FakeHub {
    pub fn new(script: Vec<Value>) -> FakeHub {
        let home =
            std::env::temp_dir().join(format!("clak-hub-{}", clak_wire::json::random_hex(4)));
        std::fs::create_dir_all(&home).expect("test folder");
        let state = Arc::new(State {
            script,
            received: Mutex::new(Vec::new()),
            closed: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let control = clak_wire::location::computed_control(&home);
        listen(control, Arc::clone(&state));
        FakeHub { home, state }
    }

    /// The requests received, in order.
    pub fn requests(&self) -> Vec<Object> {
        self.state
            .received
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Waits until `count` connections have been closed by the client.
    pub fn wait_closed(&self, count: usize, within: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < within {
            if self.state.closed.load(Ordering::SeqCst) >= count {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    pub fn response(fields: Value) -> Value {
        let mut message = fields;
        if let Some(map) = message.as_object_mut() {
            map.insert("type".into(), "response".into());
        }
        message
    }

    pub fn progress(stage: &str, text: &str) -> Value {
        json!({"type": "progress", "stage": stage, "text": text})
    }
}

impl Drop for FakeHub {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        // A connection to wake up whoever is waiting for the next client.
        let _ = clak_wire::ipc::connect(
            &clak_wire::location::computed_control(&self.home),
            Duration::from_millis(200),
        );
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// One connection: reads the request, replies with the script, waits for the client to close.
fn serve<S: std::io::Read + Write>(stream: S, state: &State) {
    let mut lines = BufReader::new(stream);
    let mut first = Vec::new();
    if lines.read_until(b'\n', &mut first).unwrap_or(0) == 0 {
        return;
    }
    let Some(request) = clak_wire::json::object(first.trim_ascii_end()) else {
        return;
    };
    if let Ok(mut received) = state.received.lock() {
        received.push(request);
    }
    for message in &state.script {
        let _ = lines
            .get_mut()
            .write_all(&clak_wire::json::encode_line(message));
    }
    let _ = lines.get_mut().flush();
    // Until the client closes: that is how, once time is up, it withdraws the request.
    let mut rest = Vec::new();
    while lines.read_until(b'\n', &mut rest).is_ok_and(|n| n > 0) {
        rest.clear();
    }
    state.closed.fetch_add(1, Ordering::SeqCst);
}

#[cfg(windows)]
fn listen(control: String, state: Arc<State>) {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let wide: Vec<u16> = control.encode_utf16().chain(Some(0)).collect();
    // Handles cross threads as numbers: a raw pointer is not Send.
    let invalid = INVALID_HANDLE_VALUE as isize;
    let create = move || -> isize {
        // SAFETY: null-terminated name; no security attributes.
        unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                65536,
                65536,
                0,
                std::ptr::null(),
            ) as isize
        }
    };
    // The first instance exists before the test asks anything.
    let mut current = create();
    assert!(current != invalid, "cannot create the test pipe");
    std::thread::spawn(move || {
        loop {
            // SAFETY: the handle is an instance of the pipe just created.
            let connected = unsafe { ConnectNamedPipe(current as _, std::ptr::null_mut()) } != 0
                || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            if state.stop.load(Ordering::SeqCst) {
                return;
            }
            // The next instance before replying: the client cannot come back before it has
            // read the reply, and always finds someone listening.
            let next = create();
            if connected {
                // SAFETY: the handle is ours, and from here on the File owns it.
                let file = unsafe { std::fs::File::from_raw_handle(current as _) };
                let state = Arc::clone(&state);
                std::thread::spawn(move || serve(file, &state));
            }
            if next == invalid {
                return;
            }
            current = next;
        }
    });
}

#[cfg(unix)]
fn listen(control: String, state: Arc<State>) {
    let listener =
        std::os::unix::net::UnixListener::bind(&control).expect("cannot create the test socket");
    std::thread::spawn(move || {
        for client in listener.incoming() {
            if state.stop.load(Ordering::SeqCst) {
                return;
            }
            let Ok(client) = client else { continue };
            let state = Arc::clone(&state);
            std::thread::spawn(move || serve(client, &state));
        }
    });
}
