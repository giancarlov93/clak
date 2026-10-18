//! clak: an agent uses the programs open on the user's computer.
//!
//! Every invocation is a request to the hub (the Clak app in the tray, or in the menu
//! bar on the Mac). Who makes it, the hub reads from the operating system: this process,
//! the shell that launched it, the agent that launched the shell. The hub asks for
//! permissions, with its own window; meanwhile the command waits, and says so on stderr.

use crate::attachments;
use crate::hub::{self, HubUnavailable};
use crate::invocation::Invocation;
use crate::output::{self, Exit};
use crate::render::{self, Style};
use crate::shell::Shell;
use crate::util::{Object, flag, objects, pretty, str_of};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read, Write};
use std::path::Path;

/// The version `clak version` prints.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// This process's command line, with the real streams.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // Only if it comes from a pipe or a file: from the terminal it would wait for the
    // keyboard, and the agent would hang.
    let stdin = || {
        let mut input = std::io::stdin();
        if input.is_terminal() {
            return None;
        }
        let mut bytes = Vec::new();
        let _ = input.read_to_end(&mut bytes);
        Some(decode_stdin(&bytes))
    };
    // Always UTF-8, written to the raw streams: the console codepage stays the one of the
    // user's terminal.
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    let exit = run(
        &args,
        stdin,
        &mut out,
        &mut err,
        &clak_wire::location::home(),
        &Shell::current(),
    );
    let _ = out.flush();
    let _ = err.flush();
    exit
}

/// Standard input as text: UTF-8, or UTF-16 if it has its BOM.
fn decode_stdin(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return crate::invocation::utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return crate::invocation::utf16(rest, u16::from_be_bytes);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// The whole command line, with the streams, the hub folder and the shell passed in from
/// outside: the tests run it end to end. Returns the exit code.
pub fn run<S: AsRef<str>>(
    args: &[S],
    stdin: impl FnOnce() -> Option<String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    home: &Path,
    shell: &Shell,
) -> i32 {
    let mut invocation = match Invocation::parse(args, stdin, shell) {
        Ok(invocation) => invocation,
        Err(problem) => {
            // There is no invocation, but there is --json: a usage error goes in the envelope too.
            if args.iter().any(|a| a.as_ref() == "--json") {
                let _ = writeln!(
                    out,
                    "{}",
                    output::failure(Exit::USAGE, Some("bad_request"), &problem)
                );
            }
            let _ = writeln!(err, "clak: {problem}");
            let _ = writeln!(err, "Help: clak -h");
            return Exit::USAGE;
        }
    };
    for warning in &invocation.warnings {
        let _ = writeln!(err, "clak: warning: {warning}");
    }

    match invocation.op.as_str() {
        "help" => {
            let _ = out.write_all(output::help().as_bytes());
            return Exit::OK;
        }
        "version" => {
            let _ = writeln!(out, "clak {VERSION}");
            return Exit::OK;
        }
        "list" => {
            for line in shell.advice() {
                let _ = writeln!(err, "clak: {line}");
            }
        }
        _ => {}
    }

    // "-o -": on the screen regardless, even if it is long.
    let screen = invocation.output.as_deref() == Some("-");
    if screen {
        invocation.output = None;
    }

    let mut request = Object::new();
    request.insert("op".into(), invocation.op.clone().into());
    request.insert("via".into(), "cli".into());
    if let Some(program) = &invocation.program {
        request.insert("resource".into(), program.clone().into());
    }
    if let Some(query) = &invocation.query {
        request.insert("query".into(), query.clone().into());
    }
    if invocation.op == "call" {
        request.insert(
            "tool".into(),
            invocation
                .command
                .clone()
                .map_or(Value::Null, Value::String),
        );
        if let Some(arguments) = &invocation.arguments {
            request.insert("arguments".into(), Value::Object(arguments.clone()));
        }
        if let Some(flags) = invocation.flags.as_ref().filter(|f| !f.is_empty()) {
            request.insert("flags".into(), Value::Array(flags.clone()));
        }
    }

    let json = invocation.json;
    let fail = |out: &mut dyn Write,
                err: &mut dyn Write,
                exit: i32,
                code: Option<&str>,
                error: &str|
     -> i32 {
        if json {
            let _ = writeln!(out, "{}", output::failure(exit, code, error));
        }
        let _ = writeln!(err, "clak: {error}");
        exit
    };

    // The waiting lines go to stderr: whoever watches the agent knows what it waits for,
    // and stdout stays the result.
    let asked = hub::ask(request, hub::seconds(invocation.wait), home, |message| {
        if let Some(text) = str_of(message, "text").filter(|t| !t.is_empty()) {
            let _ = writeln!(err, "clak: {text}");
        }
    });
    let mut response = match asked {
        Ok(response) => response,
        Err(HubUnavailable(message)) => {
            return fail(out, err, Exit::UNAVAILABLE, Some("unavailable"), &message);
        }
    };

    response.remove("type");
    if !flag(&response, "ok") {
        let code = str_of(&response, "code");
        return fail(
            out,
            err,
            Exit::of(code),
            code,
            str_of(&response, "error").unwrap_or("unknown error"),
        );
    }
    response.remove("ok");

    let text: String;
    let result: Option<Value>;
    let mut failed = false;
    let mut saved = false; // the files are already in place, -o included
    match invocation.op.as_str() {
        "status" => {
            let name = response
                .get("agent")
                .and_then(|a| a.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            text = if flag(&response, "authorized") {
                format!(
                    "Connected to Clak as {name}. Connected programs: {}. List them: clak list",
                    node_text(response.get("resources"))
                )
            } else {
                format!(
                    "Not connected yet: the first 'clak list' will ask the user for permission for {name}."
                )
            };
            result = Some(Value::Object(response.clone()));
        }
        "list" => {
            text = render::programs(&response, Style::Cli);
            result = response.get("resources").cloned();
        }
        "tools" if invocation.command.is_some() => {
            let command = invocation.command.as_deref().unwrap_or("");
            let program = invocation.program.as_deref().unwrap_or("");
            let tools = objects(response.get("tools"));
            let Some(usage) = render::command(&response, command) else {
                let near = clak_wire::names::closest(
                    Some(command),
                    tools.iter().filter_map(|t| str_of(t, "name")),
                );
                let hint = near
                    .map(|n| format!(": did you mean '{n}'?"))
                    .unwrap_or_else(|| ".".into());
                return fail(
                    out,
                    err,
                    Exit::NOT_FOUND,
                    Some("not_found"),
                    &format!(
                        "'{program}' has no command '{command}'{hint} List: clak {program} list"
                    ),
                );
            };
            text = usage;
            result = tools
                .into_iter()
                .find(|t| str_of(t, "name") == Some(command))
                .map(|t| Value::Object(t.clone()));
        }
        "tools" => {
            text = render::commands(&response, Style::Cli, invocation.help);
            result = Some(Value::Object(response.clone()));
        }
        _ => {
            failed = flag(&response, "is_error");
            let content = response.get("content");
            match render::blocks(content) {
                Some(blocks) if blocks.iter().any(|b| str_of(b, "type") != Some("text")) => {
                    // Files go to disk even with --json: nobody reads them in base64 on the screen.
                    let mut files = Vec::new();
                    text = match attachments::save(&blocks, &invocation, Some(&mut files)) {
                        Ok(text) => text,
                        Err(problem) => {
                            return fail(out, err, Exit::USAGE, Some("unwritable"), &problem);
                        }
                    };
                    saved = true;
                    let texts: Vec<&str> = blocks
                        .iter()
                        .filter(|b| !render::is_binary(b))
                        .filter_map(|b| {
                            str_of(b, "text").or_else(|| {
                                b.get("resource")
                                    .and_then(|r| r.get("text"))
                                    .and_then(Value::as_str)
                            })
                        })
                        .collect();
                    let joined = if texts.is_empty() {
                        Value::Null
                    } else {
                        Value::String(texts.join("\n"))
                    };
                    result = Some(json!({"text": joined, "files": files}));
                }
                _ => {
                    text = render::result(content);
                    result = Some(output::value(content, &text));
                }
            }
        }
    }

    if failed {
        // The program wrote the error: it goes to whoever asked, with a code that says so.
        if json {
            let _ = writeln!(
                out,
                "{}",
                output::failure(Exit::TOOL_ERROR, Some("tool_error"), &text)
            );
        }
        let _ = writeln!(
            err,
            "clak: '{} {}' returned an error:",
            invocation.program.as_deref().unwrap_or(""),
            invocation.command.as_deref().unwrap_or("")
        );
        let shown = if screen {
            text
        } else {
            output::fit(&text, &invocation, ".txt")
        };
        let _ = writeln!(err, "{shown}");
        return Exit::TOOL_ERROR;
    }

    let mut printed = if json { output::success(result) } else { text };
    match invocation.output.as_deref() {
        Some(path) if !saved => {
            if let Err(e) = std::fs::write(path, printed.as_bytes()) {
                return fail(
                    out,
                    err,
                    Exit::USAGE,
                    Some("unwritable"),
                    &format!("cannot write {path}: {e}"),
                );
            }
            let full = crate::util::full_path(path);
            let _ = writeln!(
                out,
                "Written to {} ({} bytes).",
                full.display(),
                printed.len()
            );
        }
        _ => {
            if !screen {
                printed = if json {
                    output::fit_json(&printed, &invocation)
                } else {
                    output::fit(&printed, &invocation, ".txt")
                };
            }
            let _ = out.write_all(printed.as_bytes());
            if !printed.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
        }
    }
    Exit::OK
}

/// A value put into a text: a string as is, a number as is,
/// an object or a list as indented JSON, nothing as nothing.
fn node_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(composite @ (Value::Array(_) | Value::Object(_))) => pretty(composite),
        Some(scalar) => crate::util::compact(scalar),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_hub::FakeHub;

    /// What a whole `clak` writes, with the fake hub behind it.
    struct Ran {
        exit: i32,
        out: String,
        err: String,
    }

    fn clak_in(home: &Path, args: &[&str], stdin: Option<&str>) -> Ran {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let stdin = stdin.map(str::to_owned);
        let exit = run(
            args,
            move || stdin,
            &mut out,
            &mut err,
            home,
            &Shell::none(),
        );
        Ran {
            exit,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    fn clak(hub: &FakeHub, args: &[&str]) -> Ran {
        clak_in(&hub.home, args, None)
    }

    fn parse(text: &str) -> Value {
        clak_wire::json::parse_str(text).unwrap()
    }

    #[test]
    fn waiting_lines_go_to_stderr_and_no_answer_exits_7() {
        let hub = FakeHub::new(vec![FakeHub::progress(
            "consent",
            "waiting for the user to answer in the Clak window…",
        )]);
        let ran = clak(&hub, &["list", "--wait", "0.3"]);
        assert_eq!(ran.exit, 7);
        assert!(
            ran.err
                .starts_with("clak: waiting for the user to answer in the Clak window…\n"),
            "{}",
            ran.err
        );
        assert!(ran.err.contains("clak: The user did not answer"));
        assert!(ran.out.is_empty());

        // With --json the envelope says the same, on stdout.
        let json = clak(&hub, &["list", "--wait", "0.3", "--json"]);
        let envelope = parse(&json.out);
        assert_eq!(
            (
                envelope["ok"].clone(),
                envelope["exit"].clone(),
                envelope["code"].clone()
            ),
            (json!(false), json!(7), json!("no_answer"))
        );
    }

    #[test]
    fn a_hub_error_has_its_exit_code_and_arrives_as_is() {
        let hub = FakeHub::new(vec![FakeHub::response(json!({
            "ok": false, "code": "bad_request",
            "error": "'run_python' has no parameter --intnet: did you mean --intent? Usage: run_python --code TEXT --intent read|write",
        }))]);
        let ran = clak(&hub, &["revit", "run_python", "--intnet", "read"]);
        assert_eq!(ran.exit, 2);
        assert!(ran.err.contains("did you mean --intent?"));
        let request = &hub.requests()[0];
        assert_eq!(
            clak_wire::json::text(&request["flags"]),
            r#"[{"name":"intnet","value":"read"}]"#
        );
        assert_eq!(request["op"], "call");
        assert_eq!(request["via"], "cli");
        assert_eq!(request["resource"], "revit");
        assert_eq!(request["tool"], "run_python");
    }

    #[test]
    fn a_misspelled_command_with_dash_h_gets_the_suggestion() {
        let hub = FakeHub::new(vec![FakeHub::response(json!({
            "ok": true, "name": "word", "app": "Word", "title": "a.docx", "allowed": true,
            "tools": [{"name": "append_text", "description": "d", "input_schema": {"type": "object"}}],
        }))]);
        let ran = clak(&hub, &["word", "apend_text", "-h"]);
        assert_eq!(ran.exit, 5);
        assert_eq!(
            ran.err,
            "clak: 'word' has no command 'apend_text': did you mean 'append_text'? List: clak word list\n"
        );
        assert!(
            clak(&hub, &["word", "zzz", "-h"])
                .err
                .contains("no command 'zzz'. List:")
        );
        let help = clak(&hub, &["word", "append_text", "-h"]);
        assert_eq!(help.exit, 0);
        assert!(help.out.starts_with("clak word append_text\n\nd\n"));
    }

    #[test]
    fn with_json_a_json_text_result_stays_json_in_the_envelope() {
        let hub = FakeHub::new(vec![FakeHub::response(
            json!({"ok": true, "content": [{"type": "text", "text": "{\"walls\": 3}"}]}),
        )]);
        let ran = clak(&hub, &["revit", "count_walls", "--json"]);
        assert_eq!(ran.exit, 0);
        assert_eq!(
            clak_wire::json::text(&parse(&ran.out)),
            r#"{"ok":true,"exit":0,"result":{"walls":3}}"#
        );
        assert_eq!(
            clak(&hub, &["revit", "count_walls"]).out,
            "{\"walls\": 3}\n"
        );
    }

    #[test]
    fn with_json_the_files_of_a_result_are_paths_not_base64() {
        let folder =
            std::env::temp_dir().join(format!("clak-files-{}", clak_wire::json::random_hex(4)));
        let hub = FakeHub::new(vec![FakeHub::response(json!({"ok": true, "content": [
            {"type": "text", "text": "Here is the view."}, {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"},
        ]}))]);
        let output = folder.join("view.png");
        let output_text = output.to_string_lossy().into_owned();
        let ran = clak(&hub, &["blender", "capture", "--json", "-o", &output_text]);
        let result = parse(&ran.out)["result"].clone();
        assert_eq!(result["text"], "Here is the view.");
        assert_eq!(
            result["files"],
            json!([{"path": output_text, "mime": "image/png", "bytes": 8}])
        );
        assert!(output.is_file());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_long_result_goes_to_a_file_and_dash_o_dash_prints_it() {
        let long = (0..5000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let hub = FakeHub::new(vec![FakeHub::response(
            json!({"ok": true, "content": long}),
        )]);
        let ran = clak(&hub, &["revit", "dump"]);
        assert!(
            ran.out
                .starts_with(&format!("The result is long ({} characters", long.len()))
        );
        let start = ran.out.find("It is in ").unwrap() + 9;
        let path = &ran.out[start..ran.out.find(": read it from there").unwrap()];
        assert!(
            Path::new(path).starts_with(attachments::folder()) && path.ends_with("-revit-dump.txt"),
            "{path}"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), long);
        let _ = std::fs::remove_file(path);

        assert_eq!(
            clak(&hub, &["revit", "dump", "-o", "-"]).out,
            format!("{long}\n")
        );
    }

    #[test]
    fn dash_o_writes_the_result_to_a_file() {
        let folder =
            std::env::temp_dir().join(format!("clak-out-{}", clak_wire::json::random_hex(4)));
        std::fs::create_dir_all(&folder).unwrap();
        let hub = FakeHub::new(vec![FakeHub::response(
            json!({"ok": true, "content": "résumé"}),
        )]);
        let file = folder.join("r.txt");
        let ran = clak(&hub, &["revit", "dump", "-o", &file.to_string_lossy()]);
        assert_eq!(ran.exit, 0);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "résumé");
        assert_eq!(
            ran.out,
            format!("Written to {} (8 bytes).\n", file.display())
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_program_error_exits_1_with_the_text_on_stderr() {
        let hub = FakeHub::new(vec![FakeHub::response(
            json!({"ok": true, "is_error": true, "content": [{"type": "text", "text": "wall not found"}]}),
        )]);
        let ran = clak(&hub, &["revit", "delete", "--json"]);
        assert_eq!(ran.exit, 1);
        assert_eq!(
            ran.err,
            "clak: 'revit delete' returned an error:\nwall not found\n"
        );
        assert_eq!(parse(&ran.out)["code"], "tool_error");
    }

    #[test]
    fn the_status_and_the_list() {
        let hub = FakeHub::new(vec![FakeHub::response(
            json!({"ok": true, "agent": {"name": "Claude Code"}, "authorized": true, "resources": 2}),
        )]);
        let ran = clak(&hub, &["status"]);
        assert_eq!(
            ran.out,
            "Connected to Clak as Claude Code. Connected programs: 2. List them: clak list\n"
        );
        let json = parse(&clak(&hub, &["status", "--json"]).out);
        assert_eq!(
            json["result"],
            json!({"agent": {"name": "Claude Code"}, "authorized": true, "resources": 2})
        );

        let hub = FakeHub::new(vec![FakeHub::response(
            json!({"ok": true, "resources": []}),
        )]);
        let ran = clak(&hub, &["list", "--json"]);
        assert_eq!(
            clak_wire::json::text(&parse(&ran.out)),
            r#"{"ok":true,"exit":0,"result":[]}"#
        );
        assert!(
            clak(&hub, &["list"])
                .out
                .starts_with("No programs connected to Clak.")
        );
    }

    #[test]
    fn a_hub_closed_by_the_user_exits_4() {
        let home =
            std::env::temp_dir().join(format!("clak-cli-{}", clak_wire::json::random_hex(4)));
        std::fs::create_dir_all(&home).unwrap();
        // The "closed by the user" marker: clak doesn't even try to start it.
        std::fs::write(clak_wire::location::pause_flag(&home), b"").unwrap();
        let ran = clak_in(&home, &["list", "--json"], None);
        assert_eq!(ran.exit, 4);
        assert!(ran.err.contains("closed by the user"));
        assert_eq!(parse(&ran.out)["code"], "unavailable");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn with_json_a_usage_error_also_goes_in_the_envelope() {
        let hub = FakeHub::new(vec![]);
        let ran = clak(&hub, &["list", "--json", "--wait", "never"]);
        assert_eq!(ran.exit, 2);
        assert_eq!(
            ran.err,
            "clak: --wait needs a number of seconds\nHelp: clak -h\n"
        );
        assert_eq!(parse(&ran.out)["code"], "bad_request");
        assert!(hub.requests().is_empty());
    }

    #[test]
    fn help_and_version_do_not_bother_the_hub() {
        let hub = FakeHub::new(vec![]);
        let help = clak(&hub, &["-h"]);
        assert_eq!((help.exit, help.out.as_str()), (0, output::help().as_str()));
        let version = clak(&hub, &["version"]);
        assert_eq!(version.out, format!("clak {VERSION}\n"));
        assert!(hub.requests().is_empty());
    }

    #[test]
    fn standard_input_is_read_as_utf8_or_utf16() {
        assert_eq!(decode_stdin("café".as_bytes()), "café");
        assert_eq!(decode_stdin(&[0xEF, 0xBB, 0xBF, b'a']), "a");
        assert_eq!(decode_stdin(&[0xFF, 0xFE, b'a', 0]), "a");
    }
}
