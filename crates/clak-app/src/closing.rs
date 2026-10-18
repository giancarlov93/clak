//! `--quit` from the installer: the running copy must be gone, not just asked to go, before
//! its files are replaced. Otherwise the installer's Restart Manager kills it, web views and
//! all, and the copy started after the update can show an empty window.

use std::path::Path;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};

/// Set on the copy started below: it only passes `--quit` on to the running one.
pub const FORWARD: &str = "CLAK_QUIT_FORWARD";

/// How long the running copy has to stop its hub and close.
const PATIENCE: Duration = Duration::from_secs(30);

/// Asks the running copies of this exe to quit, and waits until they have.
pub fn quit_and_wait(args: &[String]) -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        return 1;
    };
    let running = others(&exe);
    if running.is_empty() {
        return 0;
    }
    // The single-instance plugin passes the arguments on and exits the process: a child does it.
    let _ = std::process::Command::new(&exe)
        .args(&args[1..])
        .env(FORWARD, "1")
        .status();
    let deadline = Instant::now() + PATIENCE;
    for handle in running {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u32;
        unsafe {
            WaitForSingleObject(handle, left);
            CloseHandle(handle);
        }
    }
    0
}

/// The other processes running this very exe, as handles to wait on.
fn others(exe: &Path) -> Vec<HANDLE> {
    let me = std::process::id();
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut found = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let same_name =
                String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase() == name;
            if same_name && entry.th32ProcessID != me {
                let handle = OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ProcessID,
                );
                if !handle.is_null() {
                    if image(handle)
                        .is_some_and(|path| path.eq_ignore_ascii_case(&exe.to_string_lossy()))
                    {
                        found.push(handle);
                    } else {
                        CloseHandle(handle);
                    }
                }
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    found
}

fn image(process: HANDLE) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
    };
    (ok != 0).then(|| String::from_utf16_lossy(&buffer[..size as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::GetProcessId;

    #[test]
    fn a_running_copy_is_found() {
        // Another exe stands in for the app: its own copies are what `others` looks for.
        let cmd = Path::new(&std::env::var("SystemRoot").unwrap())
            .join("System32")
            .join("cmd.exe");
        let mut child = std::process::Command::new(&cmd)
            .args(["/c", "ping -n 3 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        let ids: Vec<u32> = others(&cmd)
            .into_iter()
            .map(|handle| unsafe {
                let id = GetProcessId(handle);
                CloseHandle(handle);
                id
            })
            .collect();
        let _ = child.kill();
        let _ = child.wait();
        assert!(ids.contains(&child.id()), "{ids:?}");
    }

    #[test]
    fn nothing_running_nothing_to_wait_for() {
        assert!(others(Path::new(r"C:\nowhere\not-a-real-program.exe")).is_empty());
    }
}
