//! An MCP server that is a program on this computer: the command line that starts it,
//! as the user writes it, and where the program is.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// How long a command line can be.
const MAX_LINE: usize = 4096;

/// The program and its arguments, already split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
}

impl Launch {
    /// A command line as written in a terminal: words separated by spaces, and quotes
    /// (double or single) to keep a word with spaces together. The backslash only counts
    /// before a double quote, so Windows paths stay as they are. No variables, no `|` or
    /// `>`: no shell in between.
    pub fn parse(line: &str) -> Result<Launch, String> {
        if line.chars().count() > MAX_LINE {
            return Err(format!(
                "The command is too long ({MAX_LINE} characters at most)."
            ));
        }
        let chars: Vec<char> = line.chars().collect();
        let mut words: Vec<String> = Vec::new();
        let mut word = String::new();
        let mut in_word = false;
        let mut quote: Option<char> = None;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            match quote {
                Some(q) if c == q => quote = None,
                Some('"') if c == '\\' && next == Some('"') => {
                    word.push('"');
                    i += 1;
                }
                Some(_) => word.push(c),
                None if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                None if c == '"' || c == '\'' => {
                    quote = Some(c);
                    in_word = true;
                }
                None if c == '\\' && next == Some('"') => {
                    word.push('"');
                    in_word = true;
                    i += 1;
                }
                None => {
                    word.push(c);
                    in_word = true;
                }
            }
            i += 1;
        }
        if quote.is_some() {
            return Err("A quote in the command isn't closed.".into());
        }
        if in_word {
            words.push(word);
        }
        let mut words = words.into_iter();
        let Some(program) = words.next().filter(|p| !p.is_empty()) else {
            return Err("A command is required: the program and its arguments, e.g. npx -y @modelcontextprotocol/server-everything.".into());
        };
        Ok(Launch {
            program,
            args: words.collect(),
        })
    }

    /// The command line to show, which [`Launch::parse`] reads back the same.
    pub fn display(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|w| quoted(w))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The program's name, without folder or extension: "agent-browser".
    pub fn short_name(&self) -> String {
        let file = self
            .program
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&self.program);
        let stem = match file.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && is_launcher_extension(ext) => stem,
            _ => file,
        };
        stem.to_owned()
    }
}

fn is_launcher_extension(ext: &str) -> bool {
    ["exe", "cmd", "bat", "com", "ps1"]
        .iter()
        .any(|e| ext.eq_ignore_ascii_case(e))
}

fn quoted(word: &str) -> String {
    if !word.is_empty()
        && !word
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        return word.to_owned();
    }
    format!("\"{}\"", word.replace('"', "\\\""))
}

/// Where the program is. A path (with a folder in it) is taken as is, if the file exists;
/// a name is looked up in the folders of `path`, on Windows with the PATHEXT extensions
/// (`.exe`, `.cmd`, …), as a terminal would.
pub fn resolve(program: &str, path: &OsStr) -> Option<PathBuf> {
    let given = Path::new(program);
    if program.contains(['/', '\\']) {
        return candidates(given).into_iter().find(|p| runnable(p));
    }
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates(&dir.join(given)))
        .find(|p| runnable(p))
}

#[cfg(windows)]
fn candidates(path: &Path) -> Vec<PathBuf> {
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let mut list = Vec::new();
    if path.extension().is_some() {
        list.push(path.to_path_buf());
    }
    for ext in extensions
        .split(';')
        .map(str::trim)
        .filter(|e| e.starts_with('.'))
    {
        let mut name = path.as_os_str().to_owned();
        name.push(ext);
        list.push(PathBuf::from(name));
    }
    list
}

#[cfg(not(windows))]
fn candidates(path: &Path) -> Vec<PathBuf> {
    vec![path.to_path_buf()]
}

#[cfg(windows)]
fn runnable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(not(windows))]
fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The PATH used to look up and start programs. On the Mac an app opened from the Finder or
/// at login has launchd's minimal one (`/usr/bin:/bin:/usr/sbin:/sbin`), without Homebrew or
/// npm: it's asked once from the user's login shell, and what the process already had is
/// appended to it.
pub fn user_path() -> OsString {
    let own = std::env::var_os("PATH").unwrap_or_default();
    #[cfg(target_os = "macos")]
    {
        static LOGIN: std::sync::OnceLock<Option<OsString>> = std::sync::OnceLock::new();
        if let Some(login) = LOGIN.get_or_init(login_shell_path) {
            return merged(login, &own);
        }
    }
    own
}

/// [`user_path`] off the hub's threads: the first time, on the Mac, it may wait for the shell.
pub async fn user_path_async() -> OsString {
    tokio::task::spawn_blocking(user_path)
        .await
        .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

/// [`resolve`] with the user's PATH.
pub async fn find(program: &str) -> Option<PathBuf> {
    let path = user_path_async().await;
    resolve(program, &path)
}

/// The folders of `first`, then those of `then` that weren't there.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn merged(first: &OsStr, then: &OsStr) -> OsString {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(first).collect();
    for dir in std::env::split_paths(then) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| first.to_owned())
}

/// The login shell's PATH (`$SHELL -l -c env`), within a few seconds; nothing if it doesn't
/// answer. Between two markers, because the shell's startup files may print other things.
#[cfg(target_os = "macos")]
fn login_shell_path() -> Option<OsString> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const MARK: &str = "__clak_env__";
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/'))
        .unwrap_or_else(|| "/bin/zsh".into());
    let mut child = Command::new(shell)
        .args([
            "-l",
            "-c",
            &format!("echo {MARK}; /usr/bin/env; echo {MARK}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let text = reader.join().ok()?;
    let inside = text.split(MARK).nth(1)?;
    inside
        .lines()
        .find_map(|l| l.strip_prefix("PATH="))
        .filter(|p| !p.is_empty())
        .map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        let launch = Launch::parse(line).unwrap();
        std::iter::once(launch.program).chain(launch.args).collect()
    }

    #[test]
    fn la_riga_di_comando_si_divide_come_in_un_terminale() {
        assert_eq!(
            words("agent-browser mcp --tools core,debug,mobile"),
            ["agent-browser", "mcp", "--tools", "core,debug,mobile"]
        );
        assert_eq!(words("  npx   -y  pkg  "), ["npx", "-y", "pkg"]);
        assert_eq!(
            words(r#""C:\Program Files\x\srv.exe" --dir 'a b' --x="1 2""#),
            [r"C:\Program Files\x\srv.exe", "--dir", "a b", "--x=1 2"]
        );
        assert_eq!(
            words(r#"srv --name \"q\" "a \"b\"""#),
            ["srv", "--name", "\"q\"", "a \"b\""]
        );
        assert_eq!(
            words(r"C:\tools\srv.exe --root C:\data\"),
            [r"C:\tools\srv.exe", "--root", r"C:\data\"]
        );
        assert_eq!(words(r#"srv """#), ["srv", ""]);
        assert!(Launch::parse("").is_err());
        assert!(Launch::parse("   ").is_err());
        assert!(Launch::parse("srv \"aperta").is_err());
    }

    #[test]
    fn la_riga_mostrata_si_rilegge_uguale() {
        for line in [
            "agent-browser mcp --tools core",
            r#""C:\Program Files\x.exe" "a b" "" 'c'"#,
            r#"srv "a \"b\"""#,
        ] {
            let launch = Launch::parse(line).unwrap();
            assert_eq!(Launch::parse(&launch.display()).unwrap(), launch, "{line}");
        }
        assert_eq!(Launch::parse("srv 'a b'").unwrap().display(), "srv \"a b\"");
    }

    #[test]
    fn il_nome_breve_del_programma() {
        let short = |line: &str| Launch::parse(line).unwrap().short_name();
        assert_eq!(short("agent-browser mcp"), "agent-browser");
        assert_eq!(short(r#""C:\Program Files\x\srv.exe""#), "srv");
        assert_eq!(short("/opt/homebrew/bin/uvx mcp-server-time"), "uvx");
        assert_eq!(short("server.py"), "server.py");
    }

    #[test]
    fn un_programma_si_cerca_nel_path() {
        let dir = std::env::temp_dir().join(format!("clak-launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(windows)]
        let file = dir.join("finto-server.cmd");
        #[cfg(not(windows))]
        let file = dir.join("finto-server");
        std::fs::write(&file, "").unwrap();
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([dir.join("non-c-e"), dir.clone()]).unwrap();
        // On Windows the extension comes from PATHEXT, in upper case: for the disk it's the same file.
        let same = |found: Option<PathBuf>| {
            found.map(|f| f.to_string_lossy().to_lowercase())
                == Some(file.to_string_lossy().to_lowercase())
        };
        assert!(same(resolve("finto-server", &path)));
        assert!(same(resolve(file.to_str().unwrap(), &OsString::new())));
        assert_eq!(resolve("altro-server", &path), None);
        assert_eq!(resolve(dir.join("altro").to_str().unwrap(), &path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn i_path_si_uniscono_senza_doppioni() {
        let a = std::env::join_paths(["/opt/homebrew/bin", "/usr/bin"]).unwrap();
        let b = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let all: Vec<PathBuf> = std::env::split_paths(&merged(&a, &b)).collect();
        assert_eq!(
            all,
            [
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin")
            ]
        );
    }
}
