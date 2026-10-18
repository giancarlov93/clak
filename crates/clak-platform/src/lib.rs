//! What depends on the OS: who is behind a process, signatures, secrets.
//!
//! The hub identifies its caller from the operating system, not from what the caller
//! says about itself (the *provenance*): which process opened the pipe or the socket,
//! which agent shows up walking its ancestors, who signed the executable. Everything
//! that touches the OS to do this lives here, behind a single API: Windows (Win32) and
//! macOS (libproc, Security.framework) for real, Linux as a minimal fallback (no
//! recognized agent, no signature, no vault for secrets).
//!
//! Nothing here returns an error or panics over a process that can't be read: a
//! provenance that can't be read is an empty object or a `None`, and the consent
//! says so. These are slow calls (they walk the processes; on Windows one may
//! launch PowerShell): the hub makes them in `spawn_blocking`, never under its lock.
//!
//! A limit not to hide: the signature is that of the *host* process. For a Word
//! add-in it attests Microsoft, for a script the interpreter, for a tab the browser.
//! It says "comes from this program", not "this code is trusted".
//!
//! # API
//!
//! Provenance:
//! - [`AgentIdentity`] `{ pid, started, exe, name, chain, signed, signer }`, with
//!   [`AgentIdentity::key`] (`"pid:start"`) and [`AgentIdentity::started_at`]. `started`
//!   is in the OS unit: FILETIME on Windows, microseconds since the epoch on the Mac.
//! - [`agent_of`]`(pid: Option<u32>, authorized: &HashSet<String>) -> Option<AgentIdentity>`:
//!   the agent behind the process that opened the control channel.
//! - [`is_alive`]`(&AgentIdentity) -> bool`: the same process as before (pid and start).
//! - [`attest_loopback`]`(peer_port: u16, local_port: u16) -> serde_json::Value`:
//!   the `origin` object of a program connected over loopback TCP. Windows:
//!   `{pid, exe, signed, signer}`; Mac: `{pid, exe, signed, signer, team}`; only
//!   `{pid}` if the executable can't be read; `{}` if the process isn't found.
//! - [`pipe_client_pid`]`(RawHandle) -> Option<u32>` (Windows, named pipe) and
//!   [`unix_peer_pid`]`(RawFd) -> Option<u32>` (Unix: `LOCAL_PEERPID` on the Mac,
//!   `SO_PEERCRED` on Linux).
//! - [`signature_of`]`(exe: &str) -> Signature`: who signed a file
//!   (`signed` = "valid" | "invalid" | "unsigned" | "unknown"), cached by
//!   path, time and size.
//! - [`process_start_time`], [`process_exe`], [`process_command_line`]: the single
//!   pieces, for whoever needs them.
//! - The [`provenance`] module has the pure rules (the agent choice
//!   [`provenance::choose`], the walks [`provenance::lineage_windows`] and
//!   [`provenance::lineage_macos`], the names, the signers), which read the processes
//!   through the [`ProcessLookup`] trait: they are tested with a fake table.
//!
//! Program permissions:
//! - [`executable_hash`]`(path: &str) -> Option<String>`: hex SHA-256 of the
//!   contents, cached by path, time and size.
//!
//! Secrets (see the [`secrets`] module for why they take this shape):
//! - [`SecretBox`]: `secret(id) -> Option<String>`, `has_secret(id) -> bool`,
//!   `set_secret(id, Option<&str>, label) -> Result<(), SecretError>`.
//! - [`FileBox`]`<C: BlobCipher>`: the secrets in a file `{id: base64(blob)}`;
//!   [`DpapiBox`] = `FileBox<Dpapi>` on Windows.
//! - `KeychainBox` on the Mac (service `dev.clak.connector`, account the id).
//! - [`MemoryBox`] for tests, [`NoBox`] where there is no vault.
//! - [`default_box`]`(home: &Path, file: &str) -> Box<dyn SecretBox>`.

use std::collections::HashSet;

mod hash;
pub mod provenance;
pub mod secrets;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;

pub use hash::executable_hash;
pub use provenance::{AgentIdentity, Ancestor, ProcessEntry, ProcessLookup, Signature};
#[cfg(target_os = "macos")]
pub use secrets::KeychainBox;
pub use secrets::{BlobCipher, FileBox, MemoryBox, NoBox, SecretBox, SecretError, default_box};
#[cfg(windows)]
pub use secrets::{Dpapi, DpapiBox};

/// The agent behind a process. The process tree is walked up from the nearest:
/// if one of the ancestors is already an authorized agent (`authorized`, the
/// `pid:start` keys) the request is its own, whatever sits in between; the walk stops
/// at the first recognized agent, which is the one to ask. If there is none and the
/// chain reaches the desktop intact, the first process that isn't a shell. None if
/// nothing certain can be said (including a broken chain).
///
/// Blocking and slow: in `spawn_blocking`.
pub fn agent_of(pid: Option<u32>, authorized: &HashSet<String>) -> Option<AgentIdentity> {
    let pid = pid?;
    #[cfg(windows)]
    {
        let processes = win::Processes::snapshot();
        let (agent, chain) = provenance::select(
            &processes,
            pid,
            authorized,
            &provenance::WINDOWS,
            provenance::lineage_windows,
        )?;
        let exe = win::exe_of(agent.pid);
        let signature = exe
            .as_deref()
            .map(signature_of)
            .unwrap_or_else(Signature::unknown);
        Some(identity(agent, chain, exe, signature))
    }
    #[cfg(target_os = "macos")]
    {
        let (agent, chain) = provenance::select(
            &mac::Processes,
            pid,
            authorized,
            &provenance::MACOS,
            provenance::lineage_macos,
        )?;
        let exe = mac::path_of(agent.pid);
        let signature = exe
            .as_deref()
            .map(|exe| mac::signature(exe, Some(agent.pid)))
            .unwrap_or_else(Signature::unknown);
        Some(identity(agent, chain, exe, signature))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (pid, authorized);
        None
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn identity(
    agent: Ancestor,
    chain: Vec<String>,
    exe: Option<String>,
    signature: Signature,
) -> AgentIdentity {
    AgentIdentity {
        pid: agent.pid,
        started: agent.started,
        exe,
        name: agent.name,
        chain,
        signed: signature.signed,
        signer: signature.signer,
    }
}

/// The same process as before: same pid and same start time.
pub fn is_alive(agent: &AgentIdentity) -> bool {
    process_start_time(agent.pid) == Some(agent.started)
}

/// The start time of a process, in the OS unit (see [`AgentIdentity`]).
pub fn process_start_time(pid: u32) -> Option<i64> {
    #[cfg(windows)]
    {
        win::start_time(pid)
    }
    #[cfg(target_os = "macos")]
    {
        mac::bsd_info(pid).map(|info| info.started)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// The full path of a process's executable.
pub fn process_exe(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        win::exe_of(pid)
    }
    #[cfg(target_os = "macos")]
    {
        mac::path_of(pid)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }
}

/// The command line of a process: a label, not proof.
pub fn process_command_line(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        win::command_line(pid)
    }
    #[cfg(target_os = "macos")]
    {
        mac::command_line(pid)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let parts: Vec<String> = raw
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        Some(parts.join(" "))
    }
}

/// Who signed a file. On Windows the embedded signature or, for system files, the
/// catalog one (asked of PowerShell); on the Mac the code signature, with the Team ID.
pub fn signature_of(exe: &str) -> Signature {
    #[cfg(windows)]
    {
        let (signed, signer) = win::signature(exe);
        Signature {
            signed,
            signer,
            team: None,
        }
    }
    #[cfg(target_os = "macos")]
    {
        mac::signature(exe, None)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = exe;
        Signature::unknown()
    }
}

/// The `origin` object for the process behind a loopback connection: the one whose
/// port `peer_port` is connected to the hub's port `local_port`.
///
/// `{}` if the process isn't found, `{pid}` if its executable can't be read, otherwise
/// `{pid, exe, signed, signer}` (and `team` on the Mac); `signer` and `team` may be null.
pub fn attest_loopback(peer_port: u16, local_port: u16) -> serde_json::Value {
    use serde_json::{Value, json};
    #[cfg(windows)]
    let pid = win::tcp_owner(peer_port, local_port);
    #[cfg(target_os = "macos")]
    let pid = mac::tcp_owner(peer_port, local_port);
    #[cfg(not(any(windows, target_os = "macos")))]
    let pid: Option<u32> = {
        let _ = (peer_port, local_port);
        None
    };
    let Some(pid) = pid else {
        return Value::Object(Default::default());
    };
    let Some(exe) = process_exe(pid).filter(|exe| !exe.is_empty()) else {
        return json!({ "pid": pid });
    };
    #[cfg(target_os = "macos")]
    {
        let signature = mac::signature(&exe, Some(pid));
        json!({ "pid": pid, "exe": exe, "signed": signature.signed, "signer": signature.signer, "team": signature.team })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let signature = signature_of(&exe);
        json!({ "pid": pid, "exe": exe, "signed": signature.signed, "signer": signature.signer })
    }
}

/// The process at the other end of a named pipe (server side), asked of the OS.
#[cfg(windows)]
pub fn pipe_client_pid(pipe: std::os::windows::io::RawHandle) -> Option<u32> {
    win::pipe_client_pid(pipe)
}

/// The process at the other end of a Unix socket, asked of the kernel.
#[cfg(unix)]
pub fn unix_peer_pid(fd: std::os::fd::RawFd) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        mac::unix_peer_pid(fd)
    }
    #[cfg(target_os = "linux")]
    {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut size = size_of::<libc::ucred>() as libc::socklen_t;
        let ok = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut size,
            )
        };
        (ok == 0 && credentials.pid > 0).then_some(credentials.pid as u32)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = fd;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn this_process_has_a_real_chain() {
        let me = std::process::id();
        let started = process_start_time(me).unwrap();
        let alive = AgentIdentity {
            pid: me,
            started,
            exe: None,
            name: String::new(),
            chain: vec![],
            signed: String::new(),
            signer: None,
        };
        assert!(is_alive(&alive));
        assert!(!is_alive(&AgentIdentity {
            started: started + 1,
            ..alive.clone()
        }));
        let at = alive.started_at().unwrap();
        let age = std::time::SystemTime::now().duration_since(at).unwrap();
        assert!(age < std::time::Duration::from_secs(3600), "{age:?}");
        assert_eq!(alive.key(), format!("{me}:{started}"));

        // Whatever runs the tests (a terminal, an agent, cargo) is further up the chain.
        #[cfg(windows)]
        let (line, _) = provenance::lineage_windows(&win::Processes::snapshot(), me);
        #[cfg(target_os = "macos")]
        let (line, _) = provenance::lineage_macos(&mac::Processes, me);
        assert_eq!(line.first().map(|a| a.pid), Some(me));
        assert!(line.len() >= 2, "{line:?}");
        // cargo is the parent of the test process.
        assert!(
            line.iter()
                .any(|a| a.file.to_lowercase().starts_with("cargo")),
            "{line:?}"
        );
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn the_agent_of_this_process() {
        let me = std::process::id();
        // Already authorized, the process itself is the agent: the chain is just itself.
        let mine: HashSet<String> = [format!("{me}:{}", process_start_time(me).unwrap())].into();
        let agent = agent_of(Some(me), &mine).unwrap();
        assert_eq!(agent.pid, me);
        assert_eq!(agent.chain.len(), 1);
        assert_eq!(agent.exe.as_deref(), process_exe(me).as_deref());
        assert_eq!(agent.signed, "unsigned");
        assert!(is_alive(&agent));
        // Without permissions: a recognized agent above the tests (Claude Code, VS Code…)
        // or, failing that, the tests themselves, which aren't a shell; none if the chain is broken.
        if let Some(agent) = agent_of(Some(me), &HashSet::new()) {
            assert!(
                agent.chain.len() == 1 || provenance::Rules::current().is_agent(&agent.name),
                "{agent:?}"
            );
            assert!(is_alive(&agent), "{agent:?}");
            eprintln!(
                "test agent: {} {:?} {} {:?} {:?}",
                agent.name, agent.exe, agent.signed, agent.signer, agent.chain
            );
        }
        assert_eq!(agent_of(None, &HashSet::new()), None);
        assert_eq!(agent_of(Some(u32::MAX - 3), &HashSet::new()), None);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn a_real_program_is_recognized_by_its_tcp_ports() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let hub = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", hub)).unwrap();
        let program = client.local_addr().unwrap().port();
        let _accepted = listener.accept().unwrap();
        let origin = attest_loopback(program, hub);
        assert_eq!(origin["pid"], serde_json::json!(std::process::id()));
        assert_eq!(
            origin["exe"].as_str(),
            process_exe(std::process::id()).as_deref()
        );
        assert_eq!(origin["signed"], "unsigned");
        assert!(origin["signer"].is_null());
        let keys: Vec<&str> = origin
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        #[cfg(windows)]
        assert_eq!(keys, ["pid", "exe", "signed", "signer"]);
        #[cfg(target_os = "macos")]
        assert_eq!(keys, ["pid", "exe", "signed", "signer", "team"]);
        // A port nobody holds: no provenance.
        assert_eq!(
            attest_loopback(program, hub.wrapping_add(1).max(1)),
            serde_json::json!({})
        );
    }
}
