//! What clak prints, besides the text for the reader: the `--json` envelope, always the
//! same, where results too long for the screen go, the exit codes and the help.

use crate::attachments;
use crate::invocation::Invocation;
use crate::render;
use crate::util::{pretty, utf16_len, utf16_prefix};
use serde_json::{Value, json};

/// The exit codes: one for each different thing the agent does next. With 3 it doesn't
/// retry, with 7 it asks the user whether they are there, with 6 it checks whether the
/// command acted anyway.
pub struct Exit;

impl Exit {
    pub const OK: i32 = 0;
    pub const TOOL_ERROR: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const REFUSED: i32 = 3;
    pub const UNAVAILABLE: i32 = 4;
    pub const NOT_FOUND: i32 = 5;
    pub const APP_TIMEOUT: i32 = 6;
    pub const NO_ANSWER: i32 = 7;

    /// The exit code for a hub error.
    pub fn of(code: Option<&str>) -> i32 {
        match code {
            Some("bad_request" | "unknown_agent") => Exit::USAGE,
            Some("not_found" | "gone") => Exit::NOT_FOUND,
            Some("app_timeout" | "wait_timeout") => Exit::APP_TIMEOUT,
            Some("no_answer") => Exit::NO_ANSWER,
            Some("no_prompt") => Exit::UNAVAILABLE,
            _ => Exit::REFUSED,
        }
    }
}

/// Above this length a result goes to a file. Agents truncate a command's output around
/// 30,000 characters, and do so without saying.
pub const LIMIT: usize = 25_000;

const PREVIEW: usize = 1_000;

/// `{ok: true, exit: 0, result}`.
pub fn success(result: Option<Value>) -> String {
    pretty(&json!({"ok": true, "exit": Exit::OK, "result": result}))
}

/// `{ok: false, exit, code, error}`.
pub fn failure(exit: i32, code: Option<&str>, error: &str) -> String {
    pretty(&json!({"ok": false, "exit": exit, "code": code, "error": error}))
}

/// A command's result as a value: a text that is JSON stays JSON (an object, not a
/// string containing an object); any other text stays text.
pub fn value(content: Option<&Value>, text: &str) -> Value {
    if let Some(content @ (Value::Array(_) | Value::Object(_))) = content
        && render::blocks(Some(content)).is_none()
    {
        return content.clone();
    }
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(parsed) = clak_wire::json::parse_str(text)
    {
        return parsed;
    }
    Value::String(text.to_owned())
}

/// The text as is, or, if it is too long, where it was written and how it starts.
pub fn fit(text: &str, invocation: &Invocation, extension: &str) -> String {
    let length = utf16_len(text);
    if length <= LIMIT {
        return text.to_owned();
    }
    // Better a text truncated by the agent than no text.
    let Ok(path) = attachments::spill(text, invocation, extension) else {
        return text.to_owned();
    };
    let mut start = utf16_prefix(text, PREVIEW);
    // Up to the last whole line, if not too much is lost: a start cut midway confuses.
    if let Some(cut) = start.rfind('\n')
        && utf16_len(&start[..cut]) > PREVIEW / 2
    {
        start = &start[..cut];
    }
    format!(
        "The result is long ({length} characters, {}): not printing it in full, the agent would truncate it silently. \
         It is in {}: read it from there, in chunks. Use -o FILE to choose where to write it, -o - to print it anyway.\n\n\
         It starts like this:\n{start}\n…\n",
        attachments::size(text.len() as u64),
        path.display()
    )
}

/// Like [`fit`], but stays JSON: the envelope says where the result is instead of holding it.
pub fn fit_json(json: &str, invocation: &Invocation) -> String {
    let length = utf16_len(json);
    if length <= LIMIT {
        return json.to_owned();
    }
    let Ok(path) = attachments::spill(json, invocation, ".json") else {
        return json.to_owned();
    };
    pretty(&json!({
        "ok": true,
        "exit": Exit::OK,
        "result_file": path.to_string_lossy(),
        "note": format!("Long response ({length} characters): it is in result_file, in the same envelope. Use -o - to print it anyway."),
    }))
}

/// The help of `clak -h`. The two lines that differ between systems are the file folder
/// and what breaks the process chain from which the hub works out who is asking.
pub fn help() -> String {
    HELP.replace("{folder}", HELP_FOLDER)
        .replace("{chain}", HELP_CHAIN)
}

#[cfg(windows)]
const HELP_FOLDER: &str = r"%TEMP%\clak";
#[cfg(not(windows))]
const HELP_FOLDER: &str = "$TMPDIR/clak";

#[cfg(windows)]
const HELP_CHAIN: &str =
    "not inside timeout or xargs (in Git Bash they break the chain). For time limits use --wait.";
#[cfg(not(windows))]
const HELP_CHAIN: &str = "not detached from the shell (in the background with &, with nohup or setsid), because an\n\
                          orphaned process breaks the chain. For time limits use --wait.";

const HELP: &str = r#"clak — use the programs open on the user's computer that are connected to Clak.

  clak list                          the connected programs
  clak <program> -h                  how to use it: its instructions, if it has any, and its commands
  clak <program> list                its commands, each with its usage line
  clak <program> search <text>       the commands that mention <text>
  clak <program> <command> -h        how to use a command
  clak <program> <command> [--parameter value ...]
  clak <program> call <command> …    (if the command is named list, search or help)
  clak status                        whether this agent is already connected

Command parameters:
  --name value, --name=value     converted as the command expects (text, number, yes/no, JSON)
  --switch                       true
  value                          unnamed: goes to the required parameters, in order
  --name @file                   the value is the file's text (UTF-8): the way to pass long text,
                                 code, accented letters. A value that starts with @ is written @@
  --name -                       the value is all of standard input
  --args '{"name": "value"}'     all arguments as JSON; '--args @file' from a file, '--args -' from stdin

Options:
  --json             the response as JSON, in an envelope that is always the same:
                     {"ok": true, "exit": 0, "result": …} or {"ok": false, "exit": N, "code": …, "error": …}
  -o, --output FILE  write the result to a file instead of the screen. If the command returns
                     a file (an image, a PDF, any binary) this is where it goes;
                     without -o it goes to {folder}, and clak prints its path.
                     A result over 25,000 characters goes to a file anyway: '-o -' prints it
  --wait SECONDS     how long to wait (including for the user's answer); default 110

Permissions: the first time an agent uses clak, and the first time it uses a program,
Clak asks the user for permission in its own window, and the command waits for the
answer; meanwhile clak says so on stderr. If the user refuses, do not retry until they
ask you to. If they do not answer, ask them whether they are at the computer.
Clak identifies who is asking by walking up the process tree: run clak directly,
{chain}

Exit codes: 0 done · 1 the program returned an error · 2 wrong usage ·
3 the user refused · 4 Clak unavailable · 5 not found ·
6 the program did not answer in time · 7 the user did not answer
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_busta_di_json_e_sempre_la_stessa_e_un_risultato_json_resta_json() {
        let ok = clak_wire::json::parse_str(&success(Some(json!({"muri": 3})))).unwrap();
        assert_eq!(
            clak_wire::json::text(&ok),
            r#"{"ok":true,"exit":0,"result":{"muri":3}}"#
        );
        let ko = clak_wire::json::parse_str(&failure(7, Some("no_answer"), "nessuno")).unwrap();
        assert_eq!(
            clak_wire::json::text(&ko),
            r#"{"ok":false,"exit":7,"code":"no_answer","error":"nessuno"}"#
        );
        // Indented by two spaces.
        assert_eq!(
            success(Some(json!(1))),
            "{\n  \"ok\": true,\n  \"exit\": 0,\n  \"result\": 1\n}"
        );

        // A text that is JSON becomes JSON; other text stays text; a value that isn't blocks stays as is.
        let blocks =
            clak_wire::json::parse_str(r#"[{"type":"text","text":"{\"muri\": 3}"}]"#).unwrap();
        assert_eq!(
            clak_wire::json::text(&value(Some(&blocks), r#"{"muri": 3}"#)),
            r#"{"muri":3}"#
        );
        assert_eq!(value(Some(&json!("ciao")), "ciao"), json!("ciao"));
        assert_eq!(value(None, "{non è JSON"), json!("{non è JSON"));
        assert_eq!(
            clak_wire::json::text(&value(Some(&json!({"a": 1})), "")),
            r#"{"a":1}"#
        );
    }

    #[test]
    fn un_risultato_troppo_lungo_va_in_un_file_e_si_dice_dove() {
        let invocation = Invocation::call("revit", "run_python");
        assert_eq!(fit("corto", &invocation, ".txt"), "corto");

        let text = (0..5000)
            .map(|i| format!("riga {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.len() > LIMIT);
        let notice = fit(&text, &invocation, ".txt");
        assert!(notice.starts_with(&format!("The result is long ({} characters", text.len())));
        assert!(notice.contains("It starts like this:\nriga 0\nriga 1\n"));
        assert!(notice.chars().count() < 2_000);
        let start = notice.find("It is in ").unwrap() + 9;
        let end = notice.find(": read it from there").unwrap();
        let path = &notice[start..end];
        assert!(path.ends_with("-revit-run_python.txt"), "{path}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
        let _ = std::fs::remove_file(path);

        // With --json it stays JSON: the envelope says where it is.
        let json = fit_json(&success(Some(Value::String(text.clone()))), &invocation);
        let envelope = clak_wire::json::parse_str(&json).unwrap();
        assert_eq!(envelope["ok"], true);
        let file = envelope["result_file"].as_str().unwrap();
        let saved = clak_wire::json::parse_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(saved["result"], Value::String(text));
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn ogni_errore_ha_il_codice_di_uscita_di_cio_che_l_agente_deve_fare_dopo() {
        assert_eq!(Exit::of(Some("denied")), Exit::REFUSED);
        assert_eq!(Exit::of(Some("no_answer")), Exit::NO_ANSWER);
        assert_eq!(Exit::NO_ANSWER, 7);
        assert_eq!(Exit::of(Some("app_timeout")), Exit::APP_TIMEOUT);
        assert_eq!(Exit::of(Some("wait_timeout")), Exit::APP_TIMEOUT);
        assert_eq!(Exit::of(Some("no_prompt")), Exit::UNAVAILABLE);
        assert_eq!(Exit::of(Some("unknown_agent")), Exit::USAGE);
        assert_eq!(Exit::of(Some("gone")), Exit::NOT_FOUND);
        assert_eq!(Exit::of(None), Exit::REFUSED);
    }

    #[test]
    fn l_aiuto_dice_comandi_uscite_e_dove_finiscono_i_file() {
        let help = help();
        assert!(help.starts_with(
            "clak — use the programs open on the user's computer that are connected to Clak.\n\n"
        ));
        assert!(
            help.ends_with("6 the program did not answer in time · 7 the user did not answer\n")
        );
        assert!(!help.contains('{') || help.contains(r#"'{"name": "value"}'"#));
        assert!(!help.contains("{folder}") && !help.contains("{chain}"));
        #[cfg(windows)]
        assert!(help.contains("without -o it goes to %TEMP%\\clak, and clak prints its path."));
        #[cfg(windows)]
        assert!(help.contains("run clak directly,\nnot inside timeout or xargs (in Git Bash they break the chain). For time limits use --wait.\n\n"));
    }
}
