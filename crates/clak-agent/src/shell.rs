//! The shell the agent launches clak from, as far as needed to avoid the two Windows
//! traps an agent falls into without noticing:
//!
//! - Git Bash (MSYS) turns arguments that start with `/` into Windows paths:
//!   `/i` becomes `I:/`, `/x` becomes `C:/Program Files/Git/x`;
//! - Windows PowerShell 5.1 sends standard input to an exe in ASCII: every accented
//!   letter arrives as `?`, and the damage is done before clak reads it.
//!
//! clak cannot prevent them: it recognizes them and says so, at the moment it matters. On
//! the Mac neither exists, and the shell is always [`Shell::none`].

use std::sync::OnceLock;

/// The agent's shell, with what is needed to recognize its traps.
pub struct Shell {
    converts: bool,
    msys_root: Option<String>,
    windows_powershell: Box<dyn Fn() -> bool + Send + Sync>,
    // Looking at the parent process is costly: it is done only when needed, once.
    known_powershell: OnceLock<bool>,
}

impl Shell {
    /// - `converts`: Git Bash, and it changes the arguments (that is, without `MSYS_NO_PATHCONV`);
    /// - `msys_root`: where Git Bash makes `/` start (`C:/Program Files/Git`);
    /// - `windows_powershell`: whether the parent is `powershell.exe`, asked only if needed.
    pub fn new(
        converts: bool,
        msys_root: Option<String>,
        windows_powershell: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Shell {
        Shell {
            converts,
            msys_root,
            windows_powershell: Box::new(windows_powershell),
            known_powershell: OnceLock::new(),
        }
    }

    /// No traps: the tests, and the Mac.
    pub fn none() -> Shell {
        Shell::new(false, None, || false)
    }

    /// The shell of this process.
    pub fn current() -> Shell {
        #[cfg(windows)]
        {
            let set = |name: &str| std::env::var(name).is_ok_and(|v| !v.is_empty());
            let msys = set("MSYSTEM") && !set("MSYS_NO_PATHCONV");
            let root = if msys {
                Shell::msys_root(std::env::var("EXEPATH").ok().as_deref())
            } else {
                None
            };
            Shell::new(msys, root, parent_is_windows_powershell)
        }
        #[cfg(not(windows))]
        Shell::none()
    }

    /// Git Bash, and it changes the arguments.
    pub fn converts(&self) -> bool {
        self.converts
    }

    /// The parent is `powershell.exe`, version 5.1. `pwsh` (7) sends UTF-8.
    pub fn windows_powershell(&self) -> bool {
        *self
            .known_powershell
            .get_or_init(|| (self.windows_powershell)())
    }

    /// Where Git Bash makes `/` start: `EXEPATH` is its `bin` folder (`C:\Program Files\Git\bin`).
    pub fn msys_root(exe_path: Option<&str>) -> Option<String> {
        let exe_path = exe_path.filter(|p| !p.is_empty())?;
        let root = exe_path.replace('\\', "/");
        let root = root.trim_end_matches('/');
        for bin in ["/usr/bin", "/bin", "/mingw64/bin"] {
            if root.len() >= bin.len() && ends_with_ignore_case(root, bin) {
                return Some(root[..root.len() - bin.len()].to_owned());
            }
        }
        Some(root.to_owned())
    }

    /// The lines to say on connecting (in `clak list`), only if the environment is one of the two.
    pub fn advice(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.converts {
            lines.push(
                "you are in Git Bash: arguments that start with / become Windows paths (/i becomes I:/). \
                 For a Windows exe with /x options put MSYS_NO_PATHCONV=1 in front, or use PowerShell. \
                 For long text, no heredocs: write it to a file and pass it with --name @file."
                    .to_owned(),
            );
        }
        if self.windows_powershell() {
            lines.push(
                "you are in Windows PowerShell 5.1: with the default settings ($OutputEncoding not UTF-8) a pipe \
                 into clak turns accented letters into '?'. For text, write it to a UTF-8 file and pass it with --name @file."
                    .to_owned(),
            );
        }
        lines
    }

    /// A command-line value that looks like a path made by Git Bash, not by the agent.
    pub fn converted(&self, value: &str, what: &str) -> Option<String> {
        if !self.converts {
            return None;
        }
        let bytes = value.as_bytes();
        let original = if bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && &value[1..] == ":/"
        {
            Some(format!("/{}", (bytes[0] as char).to_ascii_lowercase()))
        } else {
            match self.msys_root.as_deref().filter(|r| !r.is_empty()) {
                Some(root)
                    if value.len() > root.len()
                        && starts_with_ignore_case(value, &format!("{root}/")) =>
                {
                    Some(value[root.len()..].to_owned())
                }
                _ => None,
            }
        }?;
        Some(format!(
            "{what}: '{value}' looks like '{original}' turned into a Windows path by Git Bash. \
             If you meant '{original}', run the command again with MSYS_NO_PATHCONV=1 in front, or pass the value with @file."
        ))
    }

    /// Standard input from PowerShell 5.1: only ASCII, with some "?" that may have been accented letters.
    pub fn mangled(&self, text: &str, what: &str) -> Option<String> {
        (text.contains('?') && text.is_ascii() && self.windows_powershell()).then(|| {
            format!(
                "{what} -: standard input comes from Windows PowerShell 5.1, which turns every accented letter into '?'. \
                 If the text had any, they are lost: write it to a UTF-8 file and use {what} @file."
            )
        })
    }
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

fn ends_with_ignore_case(text: &str, suffix: &str) -> bool {
    text.len() >= suffix.len()
        && text
            .get(text.len() - suffix.len()..)
            .is_some_and(|end| end.eq_ignore_ascii_case(suffix))
}

/// Is the process that launched this one `powershell.exe`? The system's process snapshot
/// tells: the parent of this one, and its name.
#[cfg(windows)]
fn parent_is_windows_powershell() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    // SAFETY: the snapshot is closed at the end; PROCESSENTRY32W holds only integers and
    // carries its size in dwSize, as Process32FirstW requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return false;
        }
        let mut entries: Vec<(u32, u32, String)> = Vec::new();
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            entries.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..end]),
            ));
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);

        let me = GetCurrentProcessId();
        let Some(&(_, parent, _)) = entries.iter().find(|(pid, _, _)| *pid == me) else {
            return false;
        };
        entries
            .iter()
            .find(|(pid, _, _)| *pid == parent)
            .is_some_and(|(_, _, name)| name.eq_ignore_ascii_case("powershell.exe"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_radice_di_git_bash_si_ricava_da_exepath() {
        assert_eq!(
            Shell::msys_root(Some(r"C:\Program Files\Git\bin")).as_deref(),
            Some("C:/Program Files/Git")
        );
        assert_eq!(
            Shell::msys_root(Some(r"C:\Program Files\Git\usr\bin")).as_deref(),
            Some("C:/Program Files/Git")
        );
        assert_eq!(Shell::msys_root(None), None);
    }

    #[test]
    fn la_shell_di_questo_processo_si_legge_senza_errori() {
        // Which one it is depends on who runs the tests: it just has to be able to tell.
        let shell = Shell::current();
        let _ = shell.windows_powershell();
        let _ = shell.advice();
    }
}
