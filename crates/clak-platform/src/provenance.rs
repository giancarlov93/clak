//! The provenance rules, without an operating system.
//!
//! Which ancestor of a process is the agent is decided here, with pure functions
//! that read the processes through [`ProcessLookup`]: the real one comes from the
//! OS, the test one is a fake table. The Windows and Mac rules differ (which are the
//! shells, where the walk stops, how an app is recognized) and both live here,
//! compiled everywhere: so both can be tested from any OS.

use std::collections::HashSet;

/// The agent behind a request: the process to ask, walking up from whoever opened the
/// pipe or the socket. Pid and start time together: once a process is dead, the OS
/// hands its pid alone to another one.
///
/// `started` is in the OS unit: on Windows a FILETIME (hundreds of nanoseconds since
/// 1601), on the Mac microseconds since the epoch, as `proc_pidinfo` gives them.
/// It's for comparing, not for showing: to show it there's [`AgentIdentity::started_at`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentIdentity {
    pub pid: u32,
    pub started: i64,
    pub exe: Option<String>,
    pub name: String,
    /// The ancestors' file names, from the process that opened the channel up to the agent included.
    pub chain: Vec<String>,
    /// "valid", "invalid", "unsigned" or "unknown".
    pub signed: String,
    pub signer: Option<String>,
}

impl AgentIdentity {
    /// The key the hub remembers an agent by: `pid:start`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.pid, self.started)
    }

    /// The start time as an instant, if there is one.
    pub fn started_at(&self) -> Option<std::time::SystemTime> {
        if self.started <= 0 {
            return None;
        }
        #[cfg(windows)]
        {
            // FILETIME: hundreds of nanoseconds since January 1, 1601.
            const EPOCH: i64 = 116_444_736_000_000_000;
            let since = self.started.checked_sub(EPOCH)?;
            if since < 0 {
                return None;
            }
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(since as u64 * 100))
        }
        #[cfg(not(windows))]
        {
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_micros(self.started as u64))
        }
    }
}

/// An ancestor of the process that opened the channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ancestor {
    pub pid: u32,
    pub started: i64,
    /// The executable's file name: `bash.exe`, `zsh`, `claude`, `Terminal`.
    pub file: String,
    /// The name the user recognizes (see [`friendly_name_windows`], [`friendly_name_macos`]).
    pub name: String,
}

impl Ancestor {
    pub fn key(&self) -> String {
        format!("{}:{}", self.pid, self.started)
    }
}

/// A row of the process table: the parent and the executable's short name
/// (Toolhelp on Windows, `pbi_comm` on the Mac).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEntry {
    pub parent: u32,
    pub name: String,
}

/// How processes are read. The real one belongs to the OS; in tests, a fake table.
///
/// Any answer may be missing: a process that just exited, or belongs to another user,
/// can't be read, and a provenance that can't be read isn't an error but an "I don't know".
pub trait ProcessLookup {
    /// The parent and the short name.
    fn entry(&self, pid: u32) -> Option<ProcessEntry>;
    /// The start time, in the OS unit (see [`AgentIdentity`]).
    fn start_time(&self, pid: u32) -> Option<i64>;
    /// The full path of the executable.
    fn exe(&self, pid: u32) -> Option<String>;
    /// The command line. Only used to name interpreters: a label, not proof.
    fn command_line(&self, pid: u32) -> Option<String>;
}

/// An OS's rules: which are the shells, which processes the walk never goes past,
/// which agents are recognized, and what a process is called.
pub struct Rules {
    /// The shells, and our own executables: when no agent is found, the question
    /// goes to the first process that isn't one of these.
    pub shells: &'static [&'static str],
    /// The desktop, the session and the services: all the user's programs start from
    /// here. They are never the agent, and the walk doesn't go past them: a permission
    /// granted to them would cover everything.
    pub roots: &'static [&'static str],
    /// The recognized agents, by name: walking up, the first one stops the walk.
    pub agents: &'static [&'static str],
    /// On Windows file names are case-insensitive; on the Mac they aren't.
    pub ignore_case: bool,
    pub friendly_name: fn(&str, Option<&str>) -> String,
}

impl Rules {
    pub fn is_shell(&self, file: &str) -> bool {
        self.contains(self.shells, file)
    }

    pub fn is_root(&self, file: &str) -> bool {
        self.contains(self.roots, file)
    }

    /// The comparison with agent names is exact: we assign those names ourselves.
    pub fn is_agent(&self, name: &str) -> bool {
        self.agents.contains(&name)
    }

    fn contains(&self, list: &[&str], file: &str) -> bool {
        if self.ignore_case {
            list.iter().any(|item| item.eq_ignore_ascii_case(file))
        } else {
            list.contains(&file)
        }
    }

    /// The rules of the OS we're running on.
    pub fn current() -> &'static Rules {
        #[cfg(target_os = "macos")]
        {
            &MACOS
        }
        #[cfg(not(target_os = "macos"))]
        {
            &WINDOWS
        }
    }
}

pub static WINDOWS: Rules = Rules {
    shells: &[
        "clak.exe",
        "clak-shim.exe",
        "cmd.exe",
        "conhost.exe",
        "powershell.exe",
        "pwsh.exe",
        "bash.exe",
        "sh.exe",
        "dash.exe",
        "zsh.exe",
        "fish.exe",
        "env.exe",
        "winpty-agent.exe",
    ],
    roots: &[
        "explorer.exe",
        "svchost.exe",
        "services.exe",
        "wininit.exe",
        "winlogon.exe",
        "userinit.exe",
        "sihost.exe",
        "runtimebroker.exe",
        "taskhostw.exe",
        "taskeng.exe",
        "smss.exe",
        "csrss.exe",
        "system",
    ],
    agents: &[
        "Claude Code",
        "Claude Desktop",
        "Codex",
        "Gemini CLI",
        "Visual Studio Code",
        "Cursor",
        "Windsurf",
    ],
    ignore_case: true,
    friendly_name: friendly_name_windows,
};

/// tmux and screen host many sessions at once: they count as a root.
pub static MACOS: Rules = Rules {
    shells: &[
        "clak",
        "clak-shim",
        "zsh",
        "bash",
        "sh",
        "dash",
        "fish",
        "tcsh",
        "csh",
        "ksh",
        "nu",
        "login",
        "env",
        "-zsh",
        "-bash",
        "-sh",
    ],
    roots: &[
        "launchd",
        "kernel_task",
        "loginwindow",
        "sshd",
        "sshd-session",
        "tmux",
        "screen",
    ],
    agents: &[
        "Claude Code",
        "Claude Desktop",
        "Codex",
        "Gemini CLI",
        "OpenCode",
        "Visual Studio Code",
        "Cursor",
        "Windsurf",
    ],
    ignore_case: false,
    friendly_name: friendly_name_macos,
};

/// Which ancestor is the agent: the nearest one already authorized, if it's reached
/// without passing another recognized agent; otherwise the first recognized agent.
/// If there is neither, and the chain reaches the desktop intact, the first process
/// that isn't a shell (or the outermost shell). A broken chain, instead, says
/// nothing: the agent could be beyond the gap, and asking permission for a
/// `timeout.exe` would only confuse.
pub fn choose(
    line: &[Ancestor],
    authorized: &HashSet<String>,
    broken: bool,
    rules: &Rules,
) -> Option<usize> {
    let mut program = None;
    for (i, ancestor) in line.iter().enumerate() {
        if authorized.contains(&ancestor.key()) || rules.is_agent(&ancestor.name) {
            return Some(i);
        }
        if program.is_none() && !rules.is_shell(&ancestor.file) {
            program = Some(i);
        }
    }
    if broken {
        return None;
    }
    program.or(if line.is_empty() {
        None
    } else {
        Some(line.len() - 1)
    })
}

/// The ancestors under the Windows rules, from the process itself upwards, up to the
/// desktop; the second value says whether the chain is broken. Every parent must have
/// started before its child: otherwise that pid belongs to another process, the real
/// parent is dead and the chain is broken — it happens, for example, with `timeout` or
/// `xargs` in Git Bash, where the intermediate shell exits right away.
pub fn lineage_windows(processes: &dyn ProcessLookup, pid: u32) -> (Vec<Ancestor>, bool) {
    let rules = &WINDOWS;
    let mut line = Vec::new();
    let Some(mut started) = processes.start_time(pid) else {
        return (line, true);
    };
    let mut current = pid;
    while line.len() < 32 {
        let Some(entry) = processes.entry(current) else {
            break;
        };
        if rules.is_root(&entry.name) {
            return (line, false);
        }
        let exe = processes.exe(current);
        let name = friendly_name_windows(
            &started_as(exe.as_deref(), &entry.name),
            processes.command_line(current).as_deref(),
        );
        line.push(Ancestor {
            pid: current,
            started,
            file: entry.name.clone(),
            name,
        });
        match processes.start_time(entry.parent) {
            Some(parent_started) if parent_started <= started => {
                current = entry.parent;
                started = parent_started;
            }
            _ => break,
        }
    }
    (line, true)
}

/// The path a Windows process started from: its directory now, with the file name it
/// had then. A running executable can't be overwritten, so self-updaters rename it
/// (Claude Code leaves `claude.exe.old.<n>`) and the path Windows reports follows the
/// rename; the process table keeps the original name.
fn started_as(exe: Option<&str>, file: &str) -> String {
    let Some(exe) = exe else {
        return file.to_string();
    };
    let current = file_name_windows(exe);
    if current.eq_ignore_ascii_case(file) {
        return exe.to_string();
    }
    format!("{}{file}", &exe[..exe.len() - current.len()])
}

/// The ancestors under the Mac rules, up to the user's session.
///
/// A chain is broken when a parent is missing, or started after its child (the pid
/// belongs to another process). On macOS, though, the usual case is a different one:
/// a process whose parent dies is reparented to launchd, and its parent becomes pid 1.
/// But every app opened from the Finder also has launchd as its parent. Reaching
/// launchd means "intact chain" only if the process right below is something launchd
/// really launches (an app, a system service); a `zsh` or a `clak` that is a direct
/// child of launchd is an orphan.
pub fn lineage_macos(processes: &dyn ProcessLookup, pid: u32) -> (Vec<Ancestor>, bool) {
    let rules = &MACOS;
    let mut line = Vec::new();
    let read = |pid: u32| -> Option<(ProcessEntry, i64)> {
        if pid == 0 {
            return None;
        }
        Some((processes.entry(pid)?, processes.start_time(pid)?))
    };
    let Some((mut info, mut started)) = read(pid) else {
        return (line, true);
    };
    let mut current = pid;
    while line.len() < 32 {
        let exe = processes.exe(current);
        let file = exe
            .as_deref()
            .map(file_name_unix)
            .unwrap_or(&info.name)
            .to_string();
        if current <= 1 || rules.is_root(&file) {
            return (line, false);
        }
        let name = friendly_name_macos(
            exe.as_deref().unwrap_or(&file),
            processes.command_line(current).as_deref(),
        );
        line.push(Ancestor {
            pid: current,
            started,
            file,
            name,
        });
        if info.parent == 1 {
            return (line, !launched_by_launchd(exe.as_deref()));
        }
        match read(info.parent) {
            Some((parent, parent_started)) if parent_started <= started => {
                current = info.parent;
                info = parent;
                started = parent_started;
            }
            _ => return (line, true),
        }
    }
    (line, true)
}

/// An executable that launchd launches on its own: an app (or one of its services), or
/// a system program. Anything else that is a child of launchd is an orphan.
pub fn launched_by_launchd(exe: Option<&str>) -> bool {
    let Some(exe) = exe else { return false };
    if exe.contains(".app/Contents/") || exe.contains(".xpc/Contents/") {
        return true;
    }
    [
        "/System/",
        "/usr/libexec/",
        "/usr/sbin/",
        "/sbin/",
        "/Library/Apple/",
    ]
    .iter()
    .any(|prefix| exe.starts_with(prefix))
}

/// The agent among the ancestors of `pid`, with the chain leading to it. The caller
/// reads the signature: the disk isn't touched here.
pub fn select(
    processes: &dyn ProcessLookup,
    pid: u32,
    authorized: &HashSet<String>,
    rules: &Rules,
    lineage: fn(&dyn ProcessLookup, u32) -> (Vec<Ancestor>, bool),
) -> Option<(Ancestor, Vec<String>)> {
    let (line, broken) = lineage(processes, pid);
    let at = choose(&line, authorized, broken, rules)?;
    let chain = line[..=at].iter().map(|a| a.file.clone()).collect();
    Some((line[at].clone(), chain))
}

// --- names ---------------------------------------------------------------------

/// The last component of a Windows path (forward slashes are accepted too).
fn file_name_windows(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn file_name_unix(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn contains_ignore_case(text: &str, what: &str) -> bool {
    text.to_lowercase().contains(&what.to_lowercase())
}

/// A name the user recognizes, on Windows. Read from the executable and, for
/// interpreters, from the command line.
pub fn friendly_name_windows(exe: &str, command_line: Option<&str>) -> String {
    let file = file_name_windows(exe);
    let command = command_line.unwrap_or("");
    let runs = |what: &str| contains_ignore_case(command, what);
    let interpreter = |f: &str| f == "node.exe" || f == "bun.exe";
    let lower = file.to_lowercase();
    let name = match lower.as_str() {
        "claude.exe"
            if contains_ignore_case(exe, r"\WindowsApps\")
                || contains_ignore_case(exe, r"\AnthropicClaude\") =>
        {
            "Claude Desktop"
        }
        "claude.exe" => "Claude Code",
        "codex.exe" => "Codex",
        "gemini.exe" => "Gemini CLI",
        f if interpreter(f) && (runs("claude-code") || runs(r"\claude")) => "Claude Code",
        f if interpreter(f) && runs("gemini") => "Gemini CLI",
        f if interpreter(f) && runs("codex") => "Codex",
        "code.exe" => "Visual Studio Code",
        "cursor.exe" => "Cursor",
        "windsurf.exe" => "Windsurf",
        "windowsterminal.exe" => "Windows Terminal",
        "explorer.exe" => "File Explorer",
        // Like Path.GetFileNameWithoutExtension: the last extension is dropped, if any.
        _ => {
            return file
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(file)
                .to_string();
        }
    };
    name.to_string()
}

/// A name the user recognizes, on the Mac.
pub fn friendly_name_macos(exe: &str, command_line: Option<&str>) -> String {
    let file = file_name_unix(exe);
    let command = command_line.unwrap_or("");
    let runs = |what: &str| contains_ignore_case(command, what);
    // Claude Code installed with its own installer is an executable named after its version.
    if exe.contains("/.local/share/claude/versions/") || file == "claude" {
        return "Claude Code".into();
    }
    if exe.contains("/Claude.app/Contents/MacOS/") && file.starts_with("Claude") {
        return "Claude Desktop".into();
    }
    if exe.contains("/Claude.app/Contents/Frameworks/") && file.starts_with("Claude Helper") {
        return "Claude Desktop".into();
    }
    match file.to_lowercase().as_str() {
        "codex" => return "Codex".into(),
        "gemini" => return "Gemini CLI".into(),
        "opencode" => return "OpenCode".into(),
        "node" | "bun" | "deno" => {
            if runs("claude-code") || runs("/claude") {
                return "Claude Code".into();
            }
            if runs("gemini") {
                return "Gemini CLI".into();
            }
            if runs("codex") {
                return "Codex".into();
            }
            if runs("opencode") {
                return "OpenCode".into();
            }
        }
        _ => {}
    }
    // VS Code and its siblings launch terminals from "Helper" processes inside their bundle.
    for (bundle, name) in [
        ("/Visual Studio Code.app/", "Visual Studio Code"),
        ("/Cursor.app/", "Cursor"),
        ("/Windsurf.app/", "Windsurf"),
        ("/OpenCode.app/", "OpenCode"),
    ] {
        if exe.contains(bundle) {
            return name.into();
        }
    }
    if exe.ends_with("/Terminal.app/Contents/MacOS/Terminal") {
        return "Terminal".into();
    }
    // An app: the bundle name, which is what the user sees in the Dock.
    if let Some(at) = exe.find(".app/Contents/MacOS/") {
        return file_name_unix(&exe[..at]).to_string();
    }
    file.to_string()
}

// --- signatures ------------------------------------------------------------------

/// Who signed an executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// "valid", "invalid", "unsigned" or "unknown".
    pub signed: String,
    pub signer: Option<String>,
    /// The developer's Team ID, on the Mac: it's their real identity, a name can be
    /// imitated. Not present on Windows.
    pub team: Option<String>,
}

impl Signature {
    pub fn unknown() -> Signature {
        Signature {
            signed: "unknown".into(),
            signer: None,
            team: None,
        }
    }

    pub fn unsigned() -> Signature {
        Signature {
            signed: "unsigned".into(),
            signer: None,
            team: None,
        }
    }
}

/// The Common Name of a distinguished name: `CN="Microsoft, Inc.", O=…` → `Microsoft, Inc.`.
/// The first non-empty `CN=`, in quotes or up to the comma.
pub fn common_name_of(subject: &str) -> Option<String> {
    let mut from = 0;
    while let Some(found) = subject[from..].find("CN=") {
        let rest = &subject[from + found + 3..];
        let quoted = rest
            .strip_prefix('"')
            .and_then(|q| q.find('"').filter(|&end| end > 0).map(|end| &q[..end]));
        // Unclosed (or empty) quotes: fall back to the unquoted form.
        let value = quoted.unwrap_or_else(|| rest.split(',').next().unwrap_or(""));
        if !value.is_empty() {
            return Some(value.trim().to_string());
        }
        // An empty "CN=": look for the next one.
        from += found + 3;
    }
    None
}

/// From the certificate to the name the user recognizes, on the Mac: "Developer ID
/// Application: Anthropic PBC (Q6L2SF6YDW)" → "Anthropic PBC"; system binaries are "Apple".
pub fn signer_name_macos(summary: Option<&str>, team: Option<&str>) -> Option<String> {
    let summary = summary.filter(|s| !s.is_empty())?;
    // "Software Signing", "macOS Software Signing": the system binaries.
    if summary.ends_with("Software Signing") {
        return Some("Apple".into());
    }
    if summary == "Apple Mac OS Application Signing" {
        return Some(
            team.map(|t| format!("App Store, team {t}"))
                .unwrap_or_else(|| "App Store".into()),
        );
    }
    let mut name = summary;
    for prefix in [
        "Developer ID Application: ",
        "Apple Development: ",
        "Apple Distribution: ",
        "Mac Developer: ",
        "3rd Party Mac Developer Application: ",
    ] {
        if let Some(rest) = name.strip_prefix(prefix) {
            name = rest;
        }
    }
    if let Some(team) = team
        && let Some(rest) = name.strip_suffix(&format!(" ({team})"))
    {
        name = rest;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // --- Windows ---

    #[test]
    fn an_agent_name_is_recognized_from_the_executable_on_windows() {
        let cases: &[(&str, Option<&str>, &str)] = &[
            (r"C:\Users\u\.local\bin\claude.exe", None, "Claude Code"),
            (
                r"C:\Program Files\WindowsApps\Claude_1.0_x64\app\claude.exe",
                None,
                "Claude Desktop",
            ),
            (
                r"C:\Program Files\nodejs\node.exe",
                Some(r"node C:\npm\node_modules\@anthropic-ai\claude-code\cli.js"),
                "Claude Code",
            ),
            (
                r"C:\Program Files\nodejs\node.exe",
                Some(r"node C:\npm\node_modules\@google\gemini-cli\index.js"),
                "Gemini CLI",
            ),
            (
                r"C:\Program Files\nodejs\node.exe",
                Some("node server.js"),
                "node",
            ),
            (r"C:\Tools\codex.exe", None, "Codex"),
            (r"C:\Tools\something.exe", None, "something"),
        ];
        for (exe, command, expected) in cases {
            assert_eq!(friendly_name_windows(exe, *command), *expected, "{exe}");
        }
    }

    /// A chain from the process that opens the pipe upwards: "clak.exe timeout.exe bash.exe claude.exe".
    fn line_windows(files: &str) -> Vec<Ancestor> {
        files
            .split(' ')
            .filter(|f| !f.is_empty())
            .enumerate()
            .map(|(i, file)| Ancestor {
                pid: 100 + i as u32,
                started: i as i64,
                file: file.into(),
                name: friendly_name_windows(&format!(r"C:\x\{file}"), None),
            })
            .collect()
    }

    fn chosen(line: Vec<Ancestor>, rules: &Rules, authorized: &[usize]) -> Option<usize> {
        let keys = authorized.iter().map(|&i| line[i].key()).collect();
        choose(&line, &keys, false, rules)
    }

    #[test]
    fn the_agent_is_the_first_recognized_walking_up_on_windows() {
        let cases: &[(&str, Option<usize>)] = &[
            ("clak.exe bash.exe claude.exe", Some(2)),
            ("clak.exe timeout.exe bash.exe claude.exe", Some(3)), // no list of wrappers to keep up to date
            ("clak.exe xargs.exe python.exe bash.exe codex.exe", Some(4)),
            ("clak-shim.exe code.exe", Some(1)),
            ("clak.exe pwsh.exe windowsterminal.exe", Some(2)), // the user, from the terminal
            ("clak.exe bash.exe python.exe", Some(2)),          // an unknown agent
            ("clak.exe cmd.exe", Some(1)),                      // shells only: the outermost
            ("CLAK.EXE Cmd.exe", Some(1)),                      // file names ignore case
            ("", None),
        ];
        for (files, expected) in cases {
            assert_eq!(
                chosen(line_windows(files), &WINDOWS, &[]),
                *expected,
                "{files}"
            );
        }
    }

    #[test]
    fn a_process_launched_by_an_authorized_agent_is_its_own() {
        let w = |files, authorized: &[usize]| chosen(line_windows(files), &WINDOWS, authorized);
        // python launched from a terminal the user already authorized: it's the terminal's.
        assert_eq!(
            w("clak.exe python.exe pwsh.exe windowsterminal.exe", &[3]),
            Some(3)
        );
        // …but a recognized agent in between asks for itself: it doesn't inherit the terminal's permission.
        assert_eq!(
            w(
                "clak.exe bash.exe claude.exe pwsh.exe windowsterminal.exe",
                &[4]
            ),
            Some(2)
        );
        // A claude launched by an authorized claude is another agent…
        assert_eq!(w("clak.exe claude.exe bash.exe claude.exe", &[3]), Some(1));
        // …and the authorized one stays itself for what it launches.
        assert_eq!(w("clak.exe timeout.exe bash.exe claude.exe", &[3]), Some(3));

        let m = |files, authorized: &[usize]| chosen(line_macos(files), &MACOS, authorized);
        assert_eq!(m("clak python3 zsh Terminal", &[3]), Some(3));
        assert_eq!(m("clak zsh claude zsh Terminal", &[4]), Some(2));
        assert_eq!(m("clak claude zsh claude", &[3]), Some(1));
        assert_eq!(m("clak timeout zsh claude", &[3]), Some(3));
    }

    #[test]
    fn a_broken_chain_does_not_ask_for_whoever_is_there() {
        let none = HashSet::new();
        // Git Bash: "timeout" is left orphaned, its parent already exited. Nothing is visible past the gap.
        let orphan = line_windows("clak.exe timeout.exe timeout.exe");
        assert_eq!(choose(&orphan, &none, true, &WINDOWS), None);
        assert_eq!(choose(&orphan, &none, false, &WINDOWS), Some(1));
        // If there's an agent before the gap, the chain is enough.
        assert_eq!(
            choose(
                &line_windows("clak.exe claude.exe bash.exe"),
                &none,
                true,
                &WINDOWS
            ),
            Some(1)
        );

        let orphan = line_macos("clak timeout timeout");
        assert_eq!(choose(&orphan, &none, true, &MACOS), None);
        assert_eq!(choose(&orphan, &none, false, &MACOS), Some(1));
        assert_eq!(
            choose(&line_macos("clak claude zsh"), &none, true, &MACOS),
            Some(1)
        );
    }

    // --- Mac ---

    #[test]
    fn an_agent_name_is_recognized_from_the_executable_on_mac() {
        let cases: &[(&str, Option<&str>, &str)] = &[
            (
                "/Users/u/.local/share/claude/versions/2.1.300",
                None,
                "Claude Code",
            ),
            (
                "/Users/u/Library/Application Support/Claude/claude-code/2.1.295/9d64/claude.app/Contents/MacOS/claude",
                None,
                "Claude Code",
            ),
            (
                "/Applications/Claude.app/Contents/MacOS/Claude",
                None,
                "Claude Desktop",
            ),
            (
                "/opt/homebrew/bin/node",
                Some("node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
                "Claude Code",
            ),
            (
                "/opt/homebrew/bin/node",
                Some("node /opt/homebrew/lib/node_modules/@google/gemini-cli/dist/index.js"),
                "Gemini CLI",
            ),
            ("/opt/homebrew/bin/node", Some("node server.js"), "node"),
            ("/opt/homebrew/bin/codex", None, "Codex"),
            (
                "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)",
                None,
                "Visual Studio Code",
            ),
            (
                "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal",
                None,
                "Terminal",
            ),
            (
                "/Applications/iTerm.app/Contents/MacOS/iTerm2",
                None,
                "iTerm",
            ),
            ("/usr/local/bin/something", None, "something"),
        ];
        for (exe, command, expected) in cases {
            assert_eq!(friendly_name_macos(exe, *command), *expected, "{exe}");
        }
    }

    fn line_macos(files: &str) -> Vec<Ancestor> {
        files
            .split(' ')
            .filter(|f| !f.is_empty())
            .enumerate()
            .map(|(i, file)| {
                let exe = if file == "Terminal" {
                    "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"
                        .to_string()
                } else {
                    format!("/x/{file}")
                };
                Ancestor {
                    pid: 100 + i as u32,
                    started: i as i64,
                    file: file.into(),
                    name: friendly_name_macos(&exe, None),
                }
            })
            .collect()
    }

    #[test]
    fn the_agent_is_the_first_recognized_walking_up_on_mac() {
        let cases: &[(&str, Option<usize>)] = &[
            ("clak zsh claude", Some(2)),
            ("clak timeout zsh claude", Some(3)),
            ("clak xargs python3 bash codex", Some(4)),
            ("clak-shim Claude", Some(1)),
            ("clak zsh login Terminal", Some(3)),
            ("clak bash python3", Some(2)),
            ("clak zsh", Some(1)),
            ("", None),
        ];
        for (files, expected) in cases {
            assert_eq!(chosen(line_macos(files), &MACOS, &[]), *expected, "{files}");
        }
    }

    #[test]
    fn launchd_launches_apps_not_shells() {
        assert!(launched_by_launchd(Some(
            "/Applications/Claude.app/Contents/MacOS/Claude"
        )));
        assert!(launched_by_launchd(Some(
            "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"
        )));
        assert!(launched_by_launchd(Some(
            "/usr/libexec/sshd-keygen-wrapper"
        )));
        assert!(!launched_by_launchd(Some("/bin/zsh")));
        assert!(!launched_by_launchd(Some("/opt/homebrew/bin/clak")));
        assert!(!launched_by_launchd(None));
    }

    #[test]
    fn signer_names() {
        assert_eq!(
            common_name_of(r#"CN="Microsoft, Inc.", O=Microsoft"#).as_deref(),
            Some("Microsoft, Inc.")
        );
        assert_eq!(
            common_name_of("CN=Microsoft Windows, O=Microsoft Corporation, C=US").as_deref(),
            Some("Microsoft Windows")
        );
        assert_eq!(common_name_of("O=Nobody"), None);
        assert_eq!(
            signer_name_macos(
                Some("Developer ID Application: Stichting Blender Foundation (68UA947AUU)"),
                Some("68UA947AUU")
            )
            .as_deref(),
            Some("Stichting Blender Foundation")
        );
        assert_eq!(
            signer_name_macos(Some("macOS Software Signing"), None).as_deref(),
            Some("Apple")
        );
        assert_eq!(
            signer_name_macos(Some("Apple Mac OS Application Signing"), Some("T1")).as_deref(),
            Some("App Store, team T1")
        );
        assert_eq!(signer_name_macos(Some(""), None), None);
    }

    // --- the walk, on a fake table ---

    /// A row: parent, short name, start, executable.
    type Row = (u32, &'static str, Option<i64>, Option<&'static str>);

    #[derive(Default)]
    struct FakeTable {
        rows: HashMap<u32, Row>,
    }

    impl FakeTable {
        fn with(
            mut self,
            pid: u32,
            parent: u32,
            name: &'static str,
            started: Option<i64>,
            exe: Option<&'static str>,
        ) -> Self {
            self.rows.insert(pid, (parent, name, started, exe));
            self
        }
    }

    impl ProcessLookup for FakeTable {
        fn entry(&self, pid: u32) -> Option<ProcessEntry> {
            self.rows.get(&pid).map(|r| ProcessEntry {
                parent: r.0,
                name: r.1.into(),
            })
        }
        fn start_time(&self, pid: u32) -> Option<i64> {
            self.rows.get(&pid).and_then(|r| r.2)
        }
        fn exe(&self, pid: u32) -> Option<String> {
            self.rows.get(&pid).and_then(|r| r.3).map(Into::into)
        }
        fn command_line(&self, _: u32) -> Option<String> {
            None
        }
    }

    #[test]
    fn the_walk_on_windows_stops_at_the_desktop() {
        let table = FakeTable::default()
            .with(4, 0, "System", Some(1), None)
            .with(
                10,
                4,
                "explorer.exe",
                Some(10),
                Some(r"C:\Windows\explorer.exe"),
            )
            .with(
                20,
                10,
                "WindowsTerminal.exe",
                Some(20),
                Some(r"C:\wt\WindowsTerminal.exe"),
            )
            .with(30, 20, "bash.exe", Some(30), Some(r"C:\git\bash.exe"))
            .with(
                40,
                30,
                "claude.exe",
                Some(40),
                Some(r"C:\Users\u\.local\bin\claude.exe"),
            )
            .with(50, 40, "bash.exe", Some(50), Some(r"C:\git\bash.exe"))
            .with(60, 50, "clak.exe", Some(60), Some(r"C:\clak\clak.exe"));
        let (line, broken) = lineage_windows(&table, 60);
        assert!(!broken);
        assert_eq!(
            line.iter().map(|a| a.file.as_str()).collect::<Vec<_>>(),
            [
                "clak.exe",
                "bash.exe",
                "claude.exe",
                "bash.exe",
                "WindowsTerminal.exe"
            ]
        );
        assert_eq!(line[4].name, "Windows Terminal");

        let (agent, chain) =
            select(&table, 60, &HashSet::new(), &WINDOWS, lineage_windows).unwrap();
        assert_eq!(
            (agent.pid, agent.started, agent.name.as_str()),
            (40, 40, "Claude Code")
        );
        assert_eq!(chain, ["clak.exe", "bash.exe", "claude.exe"]);

        // The terminal authorized, but Claude in between: it's Claude's.
        let terminal: HashSet<String> = ["20:20".to_string()].into();
        assert_eq!(
            select(&table, 60, &terminal, &WINDOWS, lineage_windows)
                .unwrap()
                .0
                .pid,
            40
        );
    }

    #[test]
    fn a_parent_born_after_the_child_breaks_the_chain() {
        // timeout's real parent is dead and its pid went to a younger process.
        let table = FakeTable::default()
            .with(10, 4, "explorer.exe", Some(10), None)
            .with(30, 10, "bash.exe", Some(500), None)
            .with(50, 30, "timeout.exe", Some(50), None)
            .with(60, 50, "clak.exe", Some(60), None);
        let (line, broken) = lineage_windows(&table, 60);
        assert!(broken);
        assert_eq!(line.len(), 2);
        assert_eq!(
            select(&table, 60, &HashSet::new(), &WINDOWS, lineage_windows),
            None
        );
        // A process that can't be read has no chain.
        assert_eq!(lineage_windows(&table, 999), (vec![], true));
        // A parent whose start time can't be read is a gap too.
        let table = table.with(30, 10, "bash.exe", None, None);
        assert!(lineage_windows(&table, 60).1);
    }

    #[test]
    fn an_agent_renamed_by_its_update_is_still_recognized() {
        // Claude Code in Windows Terminal, updated while running: its file is now
        // claude.exe.old.<n>, and the terminal's parent is gone (a broken chain).
        let old = r"C:\Users\u\.local\bin\claude.exe.old.1792429801593.49524";
        let table = FakeTable::default()
            .with(
                18680,
                12284,
                "WindowsTerminal.exe",
                Some(10),
                Some(r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal_1.24_x64\WindowsTerminal.exe"),
            )
            .with(44544, 18680, "powershell.exe", Some(20), None)
            .with(49524, 44544, "claude.exe", Some(30), Some(old))
            .with(47468, 49524, "cmd.exe", Some(40), None)
            .with(15488, 47468, "powershell.exe", Some(50), None)
            .with(60, 15488, "clak.exe", Some(60), None)
            .with(55, 49524, "bash.exe", Some(55), None)
            .with(61, 55, "clak.exe", Some(61), None);
        let (line, broken) = lineage_windows(&table, 60);
        assert!(broken);
        assert_eq!(line.len(), 6);
        for (pid, chain) in [
            (
                60,
                &["clak.exe", "powershell.exe", "cmd.exe", "claude.exe"][..],
            ),
            (61, &["clak.exe", "bash.exe", "claude.exe"][..]),
        ] {
            let (agent, found) =
                select(&table, pid, &HashSet::new(), &WINDOWS, lineage_windows).unwrap();
            assert_eq!((agent.pid, agent.name.as_str()), (49524, "Claude Code"));
            assert_eq!(found, chain);
        }

        assert_eq!(
            started_as(Some(old), "claude.exe"),
            r"C:\Users\u\.local\bin\claude.exe"
        );
        assert_eq!(
            started_as(Some(r"C:\x\Code.exe"), "code.exe"),
            r"C:\x\Code.exe"
        );
        assert_eq!(started_as(None, "claude.exe"), "claude.exe");
    }

    #[test]
    fn on_mac_launchd_is_the_end_only_under_an_app() {
        let terminal = "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal";
        let table = FakeTable::default()
            .with(1, 0, "launchd", Some(0), Some("/sbin/launchd"))
            .with(20, 1, "Terminal", Some(20), Some(terminal))
            .with(25, 20, "login", Some(25), Some("/usr/bin/login"))
            .with(30, 25, "zsh", Some(30), Some("/bin/zsh"))
            .with(40, 30, "clak", Some(40), Some("/opt/homebrew/bin/clak"));
        let (line, broken) = lineage_macos(&table, 40);
        assert!(!broken);
        assert_eq!(
            line.iter().map(|a| a.file.as_str()).collect::<Vec<_>>(),
            ["clak", "zsh", "login", "Terminal"]
        );
        assert_eq!(
            select(&table, 40, &HashSet::new(), &MACOS, lineage_macos)
                .unwrap()
                .0
                .name,
            "Terminal"
        );

        // A zsh reparented to launchd is an orphan.
        let orphan = FakeTable::default()
            .with(1, 0, "launchd", Some(0), Some("/sbin/launchd"))
            .with(30, 1, "zsh", Some(30), Some("/bin/zsh"))
            .with(40, 30, "clak", Some(40), Some("/opt/homebrew/bin/clak"));
        assert!(lineage_macos(&orphan, 40).1);
        assert_eq!(
            select(&orphan, 40, &HashSet::new(), &MACOS, lineage_macos),
            None
        );

        // tmux counts as a root.
        let tmux = FakeTable::default()
            .with(10, 1, "tmux", Some(10), Some("/opt/homebrew/bin/tmux"))
            .with(30, 10, "zsh", Some(30), Some("/bin/zsh"))
            .with(40, 30, "clak", Some(40), Some("/opt/homebrew/bin/clak"));
        assert!(!lineage_macos(&tmux, 40).1);
    }
}
