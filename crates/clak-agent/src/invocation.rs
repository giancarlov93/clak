//! A command line, understood. It doesn't talk to the hub: it only says what to ask it.
//!
//! The clak options (`--json`, `-o`, `--wait`, `--args`, `-h`) apply wherever they
//! appear; every other `--name value` is a command parameter, which the hub converts
//! according to the program's schema.
//!
//! A value can come from outside the command line: `--name -` from standard input,
//! `--name @file` from a file. The file is the safe route for long text or text with
//! accented letters: no heredoc, no quoting, no pipe encoding.

use crate::render::Which;
use crate::shell::Shell;
use crate::util::full_path;
use serde_json::{Map, Value, json};

/// What the agent asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    /// `help`, `version`, `list`, `status`, `tools` or `call`.
    pub op: String,
    pub program: Option<String>,
    pub command: Option<String>,
    pub query: Option<String>,
    /// The parameters as the agent wrote them, `{name, value}`: the hub, which has the
    /// schema, does the conversion. A value without a name has `name: null`.
    pub flags: Option<Vec<Value>>,
    pub arguments: Option<Map<String, Value>>,
    pub json: bool,
    pub output: Option<String>,
    pub wait: f64,
    pub help: bool,
    /// For `list`: the programs the agent can use (the default), the others (`--others`), or
    /// both (`--all`, `-a`).
    pub which: Which,
    /// The warnings to write to stderr before starting: values that may not be the intended ones.
    pub warnings: Vec<String>,
}

impl Invocation {
    /// Below the 120 s after which many agents interrupt a command: a clear message beats a cut.
    pub const DEFAULT_WAIT: f64 = 110.0;

    pub fn new(op: &str) -> Invocation {
        Invocation {
            op: op.to_owned(),
            program: None,
            command: None,
            query: None,
            flags: None,
            arguments: None,
            json: false,
            output: None,
            wait: Invocation::DEFAULT_WAIT,
            help: false,
            which: Which::Usable,
            warnings: Vec::new(),
        }
    }

    /// A program's command, for tests and for whoever builds the request themselves.
    pub fn call(program: &str, command: &str) -> Invocation {
        Invocation {
            program: Some(program.to_owned()),
            command: Some(command.to_owned()),
            ..Invocation::new("call")
        }
    }

    /// Understands the command line, or says why it can't.
    ///
    /// `stdin` reads all of standard input, or gives nothing if it is not redirected
    /// (there is nothing to read, and waiting for the keyboard would block the agent). It
    /// is used at most once. `shell` is the shell the line comes from, to spot its traps.
    pub fn parse<S: AsRef<str>>(
        args: &[S],
        stdin: impl FnOnce() -> Option<String>,
        shell: &Shell,
    ) -> Result<Invocation, String> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        let mut stdin = Some(stdin);
        let mut notes: Vec<String> = Vec::new();
        let mut positional: Vec<String> = Vec::new();
        let mut flags: Vec<Value> = Vec::new();
        let mut arguments: Option<Map<String, Value>> = None;
        let (mut json, mut help) = (false, false);
        let mut output: Option<String> = None;
        let mut wait = Invocation::DEFAULT_WAIT;

        // Every error also says what to do instead: the agent fixes it on the first try.
        let mut read_stdin = |what: &str, notes: &mut Vec<String>| -> Result<String, String> {
            let Some(read) = stdin.take() else {
                return Err(format!(
                    "standard input can be read only once: for {what} write the text to a file and use {what} @file"
                ));
            };
            match read() {
                None => Err(format!(
                    "{what} -: nothing to read, standard input is not coming from a pipe. \
                     Pipe the text in, or write it to a file and use {what} @file"
                )),
                Some(text) if text.is_empty() => {
                    let empty = if what == "--args" {
                        String::new()
                    } else {
                        format!("; for empty text: {what}=")
                    };
                    Err(format!(
                        "{what} -: standard input is empty. {EMPTY_REMEDY} write it to a file and use {what} @file{empty}"
                    ))
                }
                Some(text) => {
                    if let Some(warning) = shell.mangled(&text, what) {
                        notes.push(warning);
                    }
                    Ok(text)
                }
            }
        };

        let mut i = 0;
        while i < args.len() {
            let arg = args[i];
            // The value after an option, if there is one and it is not another option.
            let mut next = || -> Option<String> {
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    i += 1;
                    Some(args[i].to_owned())
                } else {
                    None
                }
            };

            match arg {
                "-h" | "--help" | "/?" => help = true,
                "--json" => json = true,
                "-o" | "--output" => match next() {
                    Some(file) => output = Some(file),
                    None => return Err("-o needs a file name (or '-' for the screen)".into()),
                },
                "--wait" => match next().as_deref().and_then(parse_seconds) {
                    Some(seconds) => wait = seconds,
                    None => return Err("--wait needs a number of seconds".into()),
                },
                "--args" => {
                    let text = match next() {
                        None => None,
                        Some(source) if source == "-" => Some(read_stdin("--args", &mut notes)?),
                        Some(source) if source.starts_with('@') => {
                            Some(read_text(&source[1..], "--args")?)
                        }
                        Some(source) => Some(source),
                    };
                    let Some(text) = text else {
                        return Err(
                            "--args needs a JSON object, '-' to read it from stdin, or @file"
                                .into(),
                        );
                    };
                    match clak_wire::json::parse_str(&text) {
                        Ok(Value::Object(parsed)) => arguments = Some(parsed),
                        _ => return Err("--args is not a JSON object".into()),
                    }
                }
                "--" => {
                    positional.extend(args[i + 1..].iter().map(|a| (*a).to_owned()));
                    i = args.len();
                }
                _ if arg.len() > 2 && arg.starts_with("--") => {
                    let body = &arg[2..];
                    let (name, mut value) = match body.find('=') {
                        Some(equals) if equals > 0 => (
                            body[..equals].to_owned(),
                            Some(body[equals + 1..].to_owned()),
                        ),
                        _ => (body.to_owned(), next()),
                    };
                    let what = format!("--{name}");
                    match value.as_deref() {
                        Some("-") => value = Some(read_stdin(&what, &mut notes)?),
                        Some(given) if given.len() > 1 && given.starts_with('@') => {
                            // "@@" is a real @: the only way to pass a value that starts with @.
                            let rest = &given[1..];
                            value = Some(if rest.starts_with('@') {
                                rest.to_owned()
                            } else {
                                read_text(rest, &what)?
                            });
                        }
                        Some(given) => {
                            if let Some(warning) = shell.converted(given, &what) {
                                notes.push(warning);
                            }
                        }
                        None => {}
                    }
                    flags.push(json!({"name": name, "value": value}));
                }
                _ => {
                    if let Some(converted) = shell.converted(arg, "a value") {
                        notes.push(converted);
                    }
                    positional.push(arg.to_owned());
                }
            }
            i += 1;
        }

        let mut options = Invocation {
            json,
            output,
            wait,
            help,
            warnings: notes,
            ..Invocation::new("help")
        };
        let Some(first) = positional.first().cloned() else {
            return Ok(options);
        };

        if matches!(first.as_str(), "list" | "status" | "help" | "version") {
            // `list -a` is `list --all`: only there, a command's value may be "-a".
            let all = first == "list" && positional.len() == 2 && positional[1] == "-a";
            if positional.len() > 1 && !all {
                return Err(format!("'{first}' takes nothing after it"));
            }
            // `list --all` and `list --others`: which programs, not parameters.
            let mut which = if all { Which::All } else { Which::Usable };
            if first == "list" {
                for flag in std::mem::take(&mut flags) {
                    let value = flag.get("value").filter(|v| !v.is_null());
                    match (flag.get("name").and_then(Value::as_str), value) {
                        (Some("all"), None) => which = Which::All,
                        (Some("others"), None) if which != Which::All => which = Which::Others,
                        (Some("others"), None) => {}
                        (Some(name @ ("all" | "others")), Some(_)) => {
                            return Err(format!("--{name} takes no value: clak list --{name}"));
                        }
                        (name, _) => {
                            return Err(format!(
                                "'list' has no --{}: clak list, clak list --others, clak list --all",
                                name.unwrap_or("")
                            ));
                        }
                    }
                }
            }
            if !flags.is_empty() || arguments.is_some() {
                return Err(format!("'{first}' takes no parameters"));
            }
            options.op = first;
            options.which = which;
            return Ok(options);
        }

        options.program = Some(first);
        if positional.len() == 1 || matches!(positional[1].as_str(), "list" | "help") {
            if positional.len() > 2 {
                return Err(format!("'{}' takes nothing after it", positional[1]));
            }
            // "clak <program> help" is "clak <program> -h": the instructions, then the commands.
            options.op = "tools".into();
            options.help = help || (positional.len() == 2 && positional[1] == "help");
            return Ok(options);
        }
        if positional[1] == "search" {
            if positional.len() < 3 {
                return Err("'search' needs something to search for".into());
            }
            options.op = "tools".into();
            options.query = Some(positional[2..].join(" "));
            return Ok(options);
        }

        let at = if positional[1] == "call" { 2 } else { 1 };
        if positional.len() <= at {
            return Err("'call' needs a command name".into());
        }
        for value in &positional[at + 1..] {
            flags.push(json!({"name": null, "value": value}));
        }
        options.op = if help { "tools" } else { "call" }.into();
        options.command = Some(positional[at].clone());
        options.flags = Some(flags);
        options.arguments = arguments;
        Ok(options)
    }
}

/// What to say when standard input is empty: on Windows the most likely cause is
/// Windows PowerShell 5.1.
#[cfg(windows)]
const EMPTY_REMEDY: &str = "For long text, or in Windows PowerShell 5.1,";
#[cfg(not(windows))]
const EMPTY_REMEDY: &str = "For long text,";

/// How to save as UTF-8, told to whoever has a file in another encoding.
#[cfg(windows)]
const UTF8_REMEDY: &str = " (in Windows PowerShell 5.1: Set-Content -Encoding utf8)";
#[cfg(not(windows))]
const UTF8_REMEDY: &str = "";

/// The seconds of `--wait`: a positive, finite number, written the English way.
fn parse_seconds(text: &str) -> Option<f64> {
    let text = text.trim();
    // Rust would accept "inf" and "nan": an infinite wait is not a wait.
    if !text
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
    {
        return None;
    }
    text.parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s > 0.0)
}

/// The text of a file. UTF-8, with or without BOM; UTF-16 if it has its BOM (that is
/// what ">" writes in Windows PowerShell 5.1). Anything else isn't guessed: it is reported.
pub fn read_text(path: &str, what: &str) -> Result<String, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{what} @{path}: file not found: {}. A value that really starts with @ is written with two: {what} @@text",
                full_path(path).display()
            ));
        }
        Err(e) => return Err(format!("{what} @{path}: cannot read the file: {e}")),
    };

    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Ok(String::from_utf8_lossy(rest).into_owned());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Ok(utf16(rest, u16::from_le_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Ok(utf16(rest, u16::from_be_bytes));
    }
    // Strict: a lenient decoding would put � in place of the wrong bytes, without saying so.
    String::from_utf8(bytes).map_err(|_| {
        format!("{what} @{path}: the file is not UTF-8, so accented letters would be garbled. Save it as UTF-8{UTF8_REMEDY}")
    })
}

/// A UTF-16 text without its BOM, in the given byte order.
pub(crate) fn utf16(bytes: &[u8], order: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| order(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str, stdin: &str) -> Invocation {
        let args: Vec<&str> = line.split(' ').filter(|a| !a.is_empty()).collect();
        let stdin = stdin.to_owned();
        Invocation::parse(&args, move || Some(stdin), &Shell::none()).unwrap()
    }

    fn p(line: &str) -> Invocation {
        parse(line, "")
    }

    fn error(args: &[&str]) -> String {
        error_with(Some(""), args)
    }

    fn error_with(stdin: Option<&str>, args: &[&str]) -> String {
        let stdin = stdin.map(str::to_owned);
        Invocation::parse(args, move || stdin, &Shell::none()).unwrap_err()
    }

    fn parse_args(args: &[&str]) -> Invocation {
        Invocation::parse(args, || None, &Shell::none()).unwrap()
    }

    fn flags(invocation: &Invocation) -> String {
        clak_wire::json::text(&Value::Array(invocation.flags.clone().unwrap()))
    }

    #[test]
    fn the_hub_verbs() {
        assert_eq!(p("").op, "help");
        assert!(p("-h").help);
        assert_eq!(p("list").op, "list");
        assert_eq!(p("status --json").op, "status");
        assert!(p("status --json").json);
        assert_eq!(p("version").op, "version");
    }

    #[test]
    fn list_shows_the_usable_programs_the_others_or_all() {
        assert_eq!(p("list").which, Which::Usable);
        assert_eq!(p("list --others").which, Which::Others);
        assert_eq!(p("list --all --json").which, Which::All);
        assert_eq!(p("list -a").which, Which::All);
        assert_eq!(p("list --others --all").which, Which::All);
        assert!(error(&["list", "--every"]).contains("clak list --others"));
        assert!(error(&["list", "--all=yes"]).contains("takes no value"));
        assert!(error(&["status", "--all"]).contains("takes no parameters"));
        // Elsewhere "-a" is a value like any other.
        assert!(flags(&p("calc add -a")).contains(r#""value":"-a""#));
    }

    #[test]
    fn a_programs_commands_and_search() {
        let word = p("word");
        assert_eq!(
            (
                word.op.as_str(),
                word.program.as_deref(),
                word.query.as_deref()
            ),
            ("tools", Some("word"), None)
        );
        let list = p("word list");
        assert_eq!(
            (list.op.as_str(), list.program.as_deref()),
            ("tools", Some("word"))
        );
        assert!(!list.help);
        // A program's help: -h, --help or help, always the commands with the instructions in front.
        for line in ["word -h", "word --help", "word help", "word list -h"] {
            let help = p(line);
            assert_eq!(
                (
                    help.op.as_str(),
                    help.program.as_deref(),
                    help.command.as_deref(),
                    help.help
                ),
                ("tools", Some("word"), None, true),
                "{line}"
            );
        }
        let call_help = p("word call help");
        assert_eq!(
            (call_help.op.as_str(), call_help.command.as_deref()),
            ("call", Some("help"))
        );
        assert!(error(&["word", "help", "other"]).contains("nothing after it"));
        let search = p("word search bold text");
        assert_eq!(
            (
                search.op.as_str(),
                search.program.as_deref(),
                search.query.as_deref()
            ),
            ("tools", Some("word"), Some("bold text"))
        );
        assert!(error(&["word", "search"]).contains("search for"));
    }

    #[test]
    fn a_command_with_named_and_unnamed_parameters() {
        let call = p("word append_text --text hello --times=3 --force free");
        assert_eq!(
            (
                call.op.as_str(),
                call.program.as_deref(),
                call.command.as_deref()
            ),
            ("call", Some("word"), Some("append_text"))
        );
        assert_eq!(
            flags(&call),
            r#"[{"name":"text","value":"hello"},{"name":"times","value":"3"},{"name":"force","value":"free"}]"#
        );

        // Explicit "call", values without a name, and a switch at the end.
        let positional = p("word call append_text one two --force");
        assert_eq!(positional.command.as_deref(), Some("append_text"));
        assert_eq!(
            flags(&positional),
            r#"[{"name":"force","value":null},{"name":null,"value":"one"},{"name":null,"value":"two"}]"#
        );
    }

    #[test]
    fn clak_options_apply_anywhere_and_do_not_become_parameters() {
        let call = p("word get_text --json -o out.txt --wait 30");
        assert!(call.json);
        assert_eq!(call.output.as_deref(), Some("out.txt"));
        assert_eq!(call.wait, 30.0);
        assert_eq!(flags(&call), "[]");
        assert_eq!(p("list").wait, Invocation::DEFAULT_WAIT);
        // -h after a command is that command's help.
        let help = p("word append_text -h");
        assert_eq!(
            (help.op.as_str(), help.command.as_deref()),
            ("tools", Some("append_text"))
        );
    }

    #[test]
    fn long_text_comes_from_stdin_only_once() {
        let call = parse("word append_text --text -", "line one\nline two");
        assert_eq!(
            flags(&call),
            r#"[{"name":"text","value":"line one\nline two"}]"#
        );
        assert!(
            error_with(Some("x"), &["word", "t", "--a", "-", "--b", "-"]).contains("only once")
        );
    }

    #[test]
    fn empty_or_missing_standard_input_is_reported_with_the_fix() {
        // Not redirected: no waiting for the keyboard, an error right away.
        let missing = error_with(None, &["word", "t", "--text", "-"]);
        assert!(missing.contains("not coming from a pipe"));
        assert!(missing.contains("--text @file"));
        let empty = error_with(Some(""), &["word", "t", "--text", "-"]);
        assert!(empty.contains("empty"));
        #[cfg(windows)]
        assert!(empty.contains("PowerShell 5.1"));
        assert!(empty.contains("--text="));
        assert_eq!(
            flags(&parse_args(&["word", "t", "--text="])),
            r#"[{"name":"text","value":""}]"#
        );
    }

    #[test]
    fn a_value_from_a_file_arrives_whole_even_with_accented_letters() {
        let folder =
            std::env::temp_dir().join(format!("clak-at-{}", clak_wire::json::random_hex(4)));
        std::fs::create_dir_all(&folder).unwrap();
        let file = |name: &str, bytes: &[u8]| -> String {
            let path = folder.join(name);
            std::fs::write(&path, bytes).unwrap();
            path.to_string_lossy().into_owned()
        };
        let script = file("script.py", "print(\"naïve café\")\n".as_bytes());
        let bom = file(
            "bom.txt",
            &[&[0xEF, 0xBB, 0xBF][..], "résumé".as_bytes()].concat(),
        );
        let utf16le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("déjà vu".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let utf16 = file("ps.txt", &utf16le); // ">" in PowerShell 5.1
        let ansi = file("ansi.txt", &[0x63, 0x61, 0x66, 0xE9]); // "café" in Windows-1252
        let json = file("args.json", r#"{"code":"é"}"#.as_bytes());

        let call = parse_args(&[
            "revit",
            "run_python",
            "--code",
            &format!("@{script}"),
            "--a",
            &format!("@{bom}"),
            &format!("--b=@{utf16}"),
        ]);
        assert_eq!(
            flags(&call),
            r#"[{"name":"code","value":"print(\"naïve café\")\n"},{"name":"a","value":"résumé"},{"name":"b","value":"déjà vu"}]"#
        );
        let arguments = parse_args(&["revit", "run_python", "--args", &format!("@{json}")])
            .arguments
            .unwrap();
        assert_eq!(arguments["code"], "é");

        // @@ is a real @; a lone @ is just an @.
        assert_eq!(
            flags(&parse_args(&[
                "chat", "send", "--to", "@@alice", "--x", "@"
            ])),
            r#"[{"name":"to","value":"@alice"},{"name":"x","value":"@"}]"#
        );

        let absent = error(&[
            "revit",
            "run_python",
            "--code",
            &format!("@{}", folder.join("missing.py").display()),
        ]);
        assert!(absent.contains("file not found"));
        assert!(absent.contains("@@"));
        assert!(
            error(&[
                "revit",
                "run_python",
                "--args",
                &format!("@{}", folder.join("missing.json").display())
            ])
            .contains("file not found")
        );
        assert!(
            error(&["revit", "run_python", "--code", &format!("@{ansi}")]).contains("is not UTF-8")
        );

        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn git_bash_and_powershell_pitfalls_are_reported_when_they_trigger() {
        let git = Shell::new(true, Some("C:/Program Files/Git".into()), || false);
        let call = Invocation::parse(
            &[
                "acad",
                "run",
                "--switch",
                "I:/",
                "--path=C:/Program Files/Git/usr/x",
                "C:/Users/a.dwg",
            ],
            || None,
            &git,
        )
        .unwrap();
        assert_eq!(call.warnings.len(), 2);
        assert!(call.warnings[0].contains("'I:/' looks like '/i'"));
        assert!(call.warnings[1].contains("'C:/Program Files/Git/usr/x' looks like '/usr/x'"));
        assert!(call.warnings[1].contains("MSYS_NO_PATHCONV=1"));
        assert!(
            Invocation::parse(&["acad", "run", "--switch", "I:/"], || None, &Shell::none())
                .unwrap()
                .warnings
                .is_empty()
        );
        assert!(git.advice().iter().any(|line| line.contains("Git Bash")));
        assert!(Shell::none().advice().is_empty());

        // PowerShell 5.1: only ASCII with some "?" means letters lost along the way.
        let powershell = Shell::new(false, None, || true);
        let mangled = Invocation::parse(
            &["word", "t", "--text", "-"],
            || Some("na?ve caf??".into()),
            &powershell,
        )
        .unwrap();
        assert_eq!(mangled.warnings.len(), 1);
        assert!(mangled.warnings[0].contains("Windows PowerShell 5.1"));
        let accented = Invocation::parse(
            &["word", "t", "--text", "-"],
            || Some("naïve café?".into()),
            &powershell,
        )
        .unwrap();
        assert!(accented.warnings.is_empty());
        let plain = Invocation::parse(
            &["word", "t", "--text", "-"],
            || Some("how are you?".into()),
            &Shell::none(),
        )
        .unwrap();
        assert!(plain.warnings.is_empty());
    }

    #[test]
    fn whole_json_arguments() {
        let call = p(r#"word append_text --args {"text":"hello"}"#);
        assert_eq!(
            clak_wire::json::text(&Value::Object(call.arguments.unwrap())),
            r#"{"text":"hello"}"#
        );
        let piped = parse("word t --args -", r#"{"n":1}"#);
        assert_eq!(
            clak_wire::json::text(&Value::Object(piped.arguments.unwrap())),
            r#"{"n":1}"#
        );
        assert!(error(&["word", "t", "--args", "[1]"]).contains("JSON object"));
    }

    #[test]
    fn usage_errors_are_reported_before_bothering_the_hub() {
        assert!(error(&["list", "other"]).contains("list"));
        assert!(error(&["status", "--x", "1"]).contains("parameters"));
        assert!(error(&["list", "--wait", "never"]).contains("seconds"));
        assert!(error(&["list", "--wait", "inf"]).contains("seconds"));
        assert!(error(&["list", "--wait", "0"]).contains("seconds"));
        assert!(error(&["list", "-o"]).contains("file"));
        assert!(error(&["word", "call"]).contains("call"));
        assert_eq!(p("list --wait 0.5").wait, 0.5);
    }
}
