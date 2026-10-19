//! Brings an agent or a program to the front: the app whose window shows it.
//!
//! The process the hub knows is rarely the one with the window: an agent on the command
//! line runs inside a terminal, and a program may connect from a helper process. So the
//! walk goes up the parents until a process with a window, the same walk that finds the
//! agent ([`clak_platform::process_lineage`]). On the Mac, Terminal and iTerm also select
//! the tab of the process, found by its tty: the first time, macOS asks whether Clak may
//! control them. On Windows a program in a console is found through its console.

use serde_json::Value;

const NO_WINDOW: &str = "no window shows it";

/// The pid behind what the page names: a session (an agent) or a program connected over
/// loopback. MCP servers, APIs and Clak's own settings have no window of their own: None.
pub fn pid_of(state: &Value, kind: &str, id: &str) -> Option<u32> {
    let list = match kind {
        "session" => "sessions",
        "resource" => "resources",
        _ => return None,
    };
    let entry = state
        .get(list)?
        .as_array()?
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(id))?;
    let pid = if kind == "session" {
        entry.get("pid")
    } else {
        let origin = entry.get("origin")?;
        if origin.get("kind").is_some() {
            return None;
        }
        origin.get("pid")
    };
    pid?.as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .filter(|&p| p > 0)
}

/// Brings to the front the window that shows `pid`. Blocking (it reads the process table,
/// and the first time on the Mac it waits for the answer about controlling the terminal).
#[cfg(target_os = "macos")]
pub fn bring(pid: u32) -> Result<(), String> {
    use objc2_app_kit::NSApplicationActivationOptions;

    let app = mac::app_of(pid).ok_or(NO_WINDOW)?;
    if let (Some(tty), Some(id)) = (clak_platform::process_tty(pid), app.bundleIdentifier()) {
        mac::select_tab(&id.to_string(), &tty);
    }
    app.unhide();
    // Only the main window: the tab just selected is in it.
    if app.activateWithOptions(NSApplicationActivationOptions::empty()) {
        Ok(())
    } else {
        Err("it has quit".into())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
    use std::path::Path;
    use std::process::Command;

    /// The app with windows that shows `pid`: the nearest one among its parents, or the
    /// one whose bundle contains it.
    pub fn app_of(pid: u32) -> Option<Retained<NSRunningApplication>> {
        clak_platform::process_lineage(pid)
            .iter()
            .find_map(|a| {
                let app =
                    NSRunningApplication::runningApplicationWithProcessIdentifier(a.pid as i32)?;
                (app.activationPolicy() == NSApplicationActivationPolicy::Regular).then_some(app)
            })
            .or_else(|| by_bundle(pid))
    }

    /// A helper outside the parent chain (launchd starts the XPC services): the app whose
    /// bundle contains it.
    pub fn by_bundle(pid: u32) -> Option<Retained<NSRunningApplication>> {
        let exe = clak_platform::process_exe(pid)?;
        let bundle = crate::app_icon::bundle_of(Path::new(&exe))?;
        NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .find(|app| {
                app.bundleURL()
                    .and_then(|url| url.path())
                    .is_some_and(|path| Path::new(&path.to_string()) == bundle)
            })
    }

    /// The tab whose tty is `tty`, in the terminals that say it. The tty arrives as an
    /// argument, never inside the script. A refusal or another terminal: nothing, the app
    /// still comes forward.
    pub fn select_tab(bundle_id: &str, tty: &str) {
        let script = match bundle_id {
            "com.apple.Terminal" => TERMINAL,
            "com.googlecode.iterm2" => ITERM,
            _ => return,
        };
        let _ = Command::new("/usr/bin/osascript")
            .args(["-e", script, tty])
            .output();
    }

    const TERMINAL: &str = r#"on run argv
  set target to item 1 of argv
  tell application id "com.apple.Terminal"
    repeat with w in windows
      repeat with t in tabs of w
        if tty of t is target then
          set miniaturized of w to false
          set selected tab of w to t
          set index of w to 1
          return
        end if
      end repeat
    end repeat
  end tell
end run"#;

    const ITERM: &str = r#"on run argv
  set target to item 1 of argv
  tell application id "com.googlecode.iterm2"
    repeat with w in windows
      repeat with t in tabs of w
        repeat with s in sessions of t
          if tty of s is target then
            select w
            tell t to select
            tell s to select
            return
          end if
        end repeat
      end repeat
    end repeat
  end tell
end run"#;
}

/// Brings to the front the window that shows `pid`. Windows lets the foreground app hand
/// the foreground over: Clak is, since the click comes from its window.
#[cfg(windows)]
pub fn bring(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    let window = clak_platform::process_lineage(pid)
        .iter()
        .find_map(|a| win::console_of(a.pid).or_else(|| win::window_of(a.pid)))
        .ok_or(NO_WINDOW)?;
    unsafe {
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        if SetForegroundWindow(window) == 0 {
            return Err("Windows didn't let it come forward".into());
        }
    }
    Ok(())
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, GetConsoleWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowThreadProcessId,
        IsWindowVisible, WS_EX_TOOLWINDOW,
    };
    use windows_sys::core::BOOL;

    /// A program in a console: the window belongs to conhost, not to it. Attaching to its
    /// console for a moment tells which one (the app has no console of its own).
    pub fn console_of(pid: u32) -> Option<HWND> {
        let window = unsafe {
            if AttachConsole(pid) == 0 {
                return None;
            }
            let window = GetConsoleWindow();
            FreeConsole();
            window
        };
        if window.is_null() {
            return None;
        }
        if unsafe { IsWindowVisible(window) } != 0 {
            return Some(window);
        }
        // Windows Terminal: the console's window is hidden, and its owner is the terminal's.
        let owner = unsafe { GetWindow(window, GW_OWNER) };
        (!owner.is_null() && unsafe { IsWindowVisible(owner) } != 0).then_some(owner)
    }

    /// The process's main window: visible, not owned by another, not a tool palette.
    pub fn window_of(pid: u32) -> Option<HWND> {
        struct Search {
            pid: u32,
            found: HWND,
        }
        unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
            let search = unsafe { &mut *(data as *mut Search) };
            let mut owner = 0u32;
            unsafe { GetWindowThreadProcessId(window, &mut owner) };
            let main = owner == search.pid
                && unsafe { IsWindowVisible(window) } != 0
                && unsafe { GetWindow(window, GW_OWNER) }.is_null()
                && unsafe { GetWindowLongW(window, GWL_EXSTYLE) } as u32 & WS_EX_TOOLWINDOW == 0;
            if main {
                search.found = window;
                return 0;
            }
            1
        }
        let mut search = Search {
            pid,
            found: std::ptr::null_mut(),
        };
        unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
        (!search.found.is_null()).then_some(search.found)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn bring(_pid: u32) -> Result<(), String> {
    Err(NO_WINDOW.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> Value {
        json!({
            "sessions": [{ "id": "s1", "pid": 4242 }],
            "resources": [
                { "id": "word", "origin": { "pid": 777, "exe": "/Applications/Word.app" } },
                { "id": "notion", "origin": { "kind": "connector", "url": "https://mcp.notion.com" } },
                { "id": "settings", "origin": { "kind": "hub" } },
                { "id": "lost", "origin": {} },
            ],
        })
    }

    #[test]
    fn an_agent_and_a_program_have_a_pid() {
        assert_eq!(pid_of(&state(), "session", "s1"), Some(4242));
        assert_eq!(pid_of(&state(), "resource", "word"), Some(777));
    }

    /// On a Mac, run from an app (a terminal, an editor): the walk finds it, and it comes
    /// to the front. Not in CI, whose runners have no app above the tests.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn the_app_above_this_process_comes_forward() {
        let app = mac::app_of(std::process::id()).expect("an app above the tests");
        println!("{:?}", app.bundleIdentifier().map(|id| id.to_string()));
        bring(std::process::id()).unwrap();
    }

    #[test]
    fn servers_settings_and_strangers_have_none() {
        assert_eq!(pid_of(&state(), "resource", "notion"), None);
        assert_eq!(pid_of(&state(), "resource", "settings"), None);
        assert_eq!(pid_of(&state(), "resource", "lost"), None);
        assert_eq!(pid_of(&state(), "session", "gone"), None);
        assert_eq!(pid_of(&state(), "grant", "s1"), None);
    }
}
