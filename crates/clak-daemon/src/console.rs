//! The hub's questions on the terminal, for `--ask`: one at a time, answered "y" or "n" on a
//! line. A command's question takes how far the yes goes: "o" this call only, "s" the
//! command for the agent's session, "a" approved in Clak; "n" denies it. "b" blocks the
//! program for the agent, where the question offers it. "Always" is only in the app's
//! window.
//!
//! Only a line typed while a question is on screen answers it. What was typed before (for a
//! question that has gone, or for none) is thrown away when the next one appears: otherwise a
//! "y" meant for one question would answer the next, unseen. A question that goes away
//! unanswered says so, and why: withdrawn, because whoever asked has gone, or expired,
//! because its time ran out.
//!
//! An MCP server's question halfway through a command: a form is asked one field at a time
//! (an empty line takes the default, or leaves an optional field out), then "Send?"; a page
//! is shown whole, to open in a browser by hand, and for a command that waits for it, an
//! empty line says the user is done there.

use clak_hub::consent::BoxFuture;
use clak_hub::elicitation::{Field, Kind};
use clak_hub::{Asks, Choices, ConsentPrompt, ConsentRequest, Elicited, Reply, Scope};
use serde_json::{Map, Value, json};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Where the questions are written.
pub type Screen = Arc<dyn Fn(&str) + Send + Sync>;

/// The hub starts the clock a moment before it asks: a question that goes away this close to
/// its time has expired.
const SLACK: Duration = Duration::from_millis(500);

pub struct ConsolePrompt {
    lines: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
    screen: Screen,
    /// The hub's time for an answer, counted from when it asks: a question waiting for its
    /// turn spends it too.
    timeout: Duration,
}

impl ConsolePrompt {
    /// On the process's terminal: the questions on stderr, the lines from stdin, read on a
    /// thread of their own so that a withdrawn question doesn't stay stuck on a read.
    pub fn new(timeout: Duration) -> ConsolePrompt {
        let (sender, receiver) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("questions".into())
            .spawn(move || {
                let mut line = String::new();
                while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
                    let _ = sender.send(std::mem::take(&mut line));
                }
            })
            .expect("thread");
        let screen: Screen = Arc::new(|text: &str| {
            let _ = write!(std::io::stderr(), "{text}");
        });
        ConsolePrompt::with(receiver, screen, timeout)
    }

    pub fn with(
        lines: mpsc::UnboundedReceiver<String>,
        screen: Screen,
        timeout: Duration,
    ) -> ConsolePrompt {
        ConsolePrompt {
            lines: Arc::new(Mutex::new(lines)),
            screen,
            timeout,
        }
    }
}

/// Whole seconds, to the nearest: 59.99 s left is 60 to the user.
fn seconds(time: Duration) -> u64 {
    time.as_secs_f64().round() as u64
}

/// A question on screen, not answered yet. If it goes away like this (the hub drops the
/// question, or cancels it) it says so.
struct Shown {
    screen: Screen,
    title: String,
    asked: Instant,
    timeout: Duration,
    answered: bool,
}

impl Drop for Shown {
    fn drop(&mut self) {
        if self.answered {
            return;
        }
        let why = if self.asked.elapsed() + SLACK >= self.timeout {
            format!("Expired, no answer within {} s", seconds(self.timeout))
        } else {
            "Withdrawn, whoever asked has gone".to_string()
        };
        (self.screen)(&format!(
            "\n    {why}: {}. What you type now answers nothing.\n",
            self.title
        ));
    }
}

/// What came from the terminal.
enum Read {
    Line(String),
    /// No more input.
    End,
    /// The question went away.
    Withdrawn,
}

async fn read(lines: &mut mpsc::UnboundedReceiver<String>, cancel: &CancellationToken) -> Read {
    tokio::select! {
        line = lines.recv() => match line {
            Some(line) => Read::Line(line.trim_end_matches(['\r', '\n']).to_owned()),
            None => Read::End,
        },
        _ = cancel.cancelled() => Read::Withdrawn,
    }
}

/// The head of a question: its title, who and what, the warning.
fn head(request: &ConsentRequest) -> String {
    let mut text = format!("\n=== {}\n", request.title);
    for line in request.who.iter().chain(&request.what) {
        text.push_str(&format!("    {}: {}\n", line.label, line.value));
    }
    if let Some(warning) = &request.warning {
        text.push_str(&format!("    WARNING: {warning}\n"));
    }
    text
}

/// A field's line: its title, what it takes, its default.
fn field_prompt(field: &Field) -> String {
    let mut text = String::new();
    if let Some(description) = &field.description {
        text.push_str(&format!("    {description}\n"));
    }
    let what = match &field.kind {
        Kind::Text { format, .. } => match format.map(|f| f.as_str()) {
            Some("email") => "an email address".to_owned(),
            Some("uri") => "an address".to_owned(),
            Some("date") => "a date, YYYY-MM-DD".to_owned(),
            Some("date-time") => "a date and time, like 2026-11-16T10:30:00+01:00".to_owned(),
            _ => "text".to_owned(),
        },
        Kind::Number { integer: true, .. } => "a whole number".to_owned(),
        Kind::Number { .. } => "a number".to_owned(),
        Kind::Boolean => "y or n".to_owned(),
        Kind::Choice { options } | Kind::Choices { options, .. } => {
            for (i, option) in options.iter().enumerate() {
                text.push_str(&format!("      {}. {}\n", i + 1, option.label));
            }
            if matches!(field.kind, Kind::Choices { .. }) {
                "numbers, separated by commas".to_owned()
            } else {
                "a number".to_owned()
            }
        }
    };
    let optional = if field.required { "" } else { ", optional" };
    let default = match &field.default {
        Some(Value::String(text)) => format!(" [{text}]"),
        Some(Value::Bool(yes)) => format!(" [{}]", if *yes { "y" } else { "n" }),
        Some(Value::Array(items)) => format!(
            " [{}]",
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(other) => format!(" [{other}]"),
        None => String::new(),
    };
    text.push_str(&format!("  {} ({what}{optional}){default}: ", field.title));
    text
}

/// What a line says for a field: `None` for nothing (empty).
fn field_value(field: &Field, line: &str) -> Result<Option<Value>, String> {
    let text = line.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let option = |pick: &str, options: &[clak_hub::elicitation::Choice]| {
        let pick = pick.trim();
        pick.parse::<usize>()
            .ok()
            .and_then(|n| options.get(n.wrapping_sub(1)))
            .or_else(|| options.iter().find(|o| o.value == pick || o.label == pick))
            .map(|o| o.value.clone())
            .ok_or_else(|| format!("“{pick}” isn't one of the options."))
    };
    Ok(Some(match &field.kind {
        Kind::Text { .. } => json!(line),
        Kind::Number { .. } => match text.parse::<f64>() {
            Ok(n) => json!(n),
            Err(_) => return Err(format!("“{}” must be a number.", field.title)),
        },
        Kind::Boolean => match text.to_lowercase().as_str() {
            "y" | "yes" => json!(true),
            "n" | "no" => json!(false),
            _ => return Err("y or n.".into()),
        },
        Kind::Choice { options } => json!(option(text, options)?),
        Kind::Choices { options, .. } => {
            let picks = text
                .split(',')
                .map(|pick| option(pick, options))
                .collect::<Result<Vec<_>, _>>()?;
            json!(picks)
        }
    }))
}

impl ConsentPrompt for ConsolePrompt {
    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<Reply, String>> {
        let lines = self.lines.clone();
        let screen = self.screen.clone();
        let timeout = self.timeout;
        let asked = Instant::now();
        Box::pin(async move {
            // One question at a time: whoever holds the receiver is the one on screen.
            let mut lines = tokio::select! {
                lines = lines.lock() => lines,
                _ = cancel.cancelled() => return Ok(Reply::Deny),
            };
            // Typed before this question was on screen: not an answer to it.
            while lines.try_recv().is_ok() {}

            let mut text = format!("\n=== {}\n", request.title);
            for line in request.who.iter().chain(&request.what) {
                text.push_str(&format!("    {}: {}\n", line.label, line.value));
            }
            for tool in &request.tools {
                let note = tool
                    .note
                    .as_deref()
                    .map(|n| format!(" ({n})"))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "    • {}{note} — {}\n",
                    tool.name, tool.description
                ));
                for change in &tool.changes {
                    text.push_str(&format!("        {}: {}\n", change.label, change.value));
                }
            }
            if !request.arguments.is_empty() {
                text.push_str("    Arguments (sent by the agent):\n");
                for line in &request.arguments {
                    text.push_str(&format!("      {}: {}\n", line.label, line.value));
                }
            }
            if let Some(warning) = &request.warning {
                text.push_str(&format!("    WARNING: {warning}\n"));
            }
            let keys = keys_of(request.choices.as_ref());
            if let Some(choices) = &request.choices {
                let mut offered: Vec<String> = choices
                    .scopes
                    .iter()
                    .map(|(scope, label)| format!("{}: {label}", key_of(*scope)))
                    .collect();
                if offered.is_empty() {
                    offered.push("y: yes".into());
                }
                offered.push("n: no".into());
                if let Some(block) = &choices.block {
                    offered.push(format!("b: {block}"));
                }
                text.push_str(&format!("    {}\n", offered.join(" · ")));
            }
            let left = seconds(timeout.saturating_sub(asked.elapsed()));
            text.push_str(&format!(
                "    {}\n    {left} s to answer.\n{} {keys}",
                request.note, request.question
            ));
            screen(&text);
            let mut shown = Shown {
                screen: screen.clone(),
                title: request.title.clone(),
                asked,
                timeout,
                answered: false,
            };
            loop {
                let line = tokio::select! {
                    line = lines.recv() => line,
                    _ = cancel.cancelled() => return Ok(Reply::Deny),
                };
                // No more input: nobody can say yes.
                let Some(line) = line else {
                    shown.answered = true;
                    screen("\n    No more input: no.\n");
                    return Ok(Reply::Deny);
                };
                match answer(&line, request.choices.as_ref()) {
                    Some(reply) => {
                        shown.answered = true;
                        return Ok(reply);
                    }
                    None => screen(&keys),
                }
            }
        })
    }

    fn elicit(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<Elicited, String>> {
        let lines = self.lines.clone();
        let screen = self.screen.clone();
        let timeout = self.timeout;
        let asked = Instant::now();
        Box::pin(async move {
            let mut lines = tokio::select! {
                lines = lines.lock() => lines,
                _ = cancel.cancelled() => return Ok(Elicited::Cancelled),
            };
            while lines.try_recv().is_ok() {}
            let left = seconds(timeout.saturating_sub(asked.elapsed()));
            screen(&format!(
                "{}    {}\n    {left} s to answer.\n{}\n",
                head(&request),
                request.note,
                request.question
            ));
            let mut shown = Shown {
                screen: screen.clone(),
                title: request.title.clone(),
                asked,
                timeout,
                answered: false,
            };
            // A yes or a no; `None` when nobody can answer any more.
            macro_rules! yes_or_no {
                ($prompt:expr) => {
                    loop {
                        screen($prompt);
                        match read(&mut lines, &cancel).await {
                            Read::Line(line) => match line.trim().to_lowercase().as_str() {
                                "y" | "yes" => break Some(true),
                                "n" | "no" => break Some(false),
                                _ => {}
                            },
                            Read::End => {
                                shown.answered = true;
                                screen("\n    No more input: no answer.\n");
                                break None;
                            }
                            Read::Withdrawn => return Ok(Elicited::Cancelled),
                        }
                    }
                };
            }
            let answer = match &request.asks {
                Some(Asks::Page(page)) => {
                    screen(&format!(
                        "    Address: {}\n    Site: {}\n",
                        page.url, page.host
                    ));
                    match yes_or_no!("Open it in your browser? [y/n] ") {
                        None => Elicited::Cancelled,
                        Some(false) => Elicited::Declined,
                        Some(true) if !page.wait => Elicited::Accepted(Map::new()),
                        Some(true) => {
                            screen(
                                "    Open the address above in your browser. When you're done there, press Enter; n cancels.\n",
                            );
                            loop {
                                match read(&mut lines, &cancel).await {
                                    Read::Line(line) => match line.trim().to_lowercase().as_str() {
                                        "" | "y" | "yes" | "done" => {
                                            break Elicited::Accepted(Map::new());
                                        }
                                        "n" | "no" => break Elicited::Declined,
                                        _ => screen("    Enter when done, n to cancel. "),
                                    },
                                    Read::End => break Elicited::Cancelled,
                                    Read::Withdrawn => {
                                        shown.answered = true;
                                        screen("\n    Done in the browser, or withdrawn.\n");
                                        return Ok(Elicited::Cancelled);
                                    }
                                }
                            }
                        }
                    }
                }
                Some(Asks::Form(form)) if !form.fields.is_empty() => {
                    let mut content = Map::new();
                    let mut ended = false;
                    'fields: for field in &form.fields {
                        loop {
                            screen(&field_prompt(field));
                            let line = match read(&mut lines, &cancel).await {
                                Read::Line(line) => line,
                                Read::End => {
                                    ended = true;
                                    break 'fields;
                                }
                                Read::Withdrawn => return Ok(Elicited::Cancelled),
                            };
                            match field_value(field, &line) {
                                Ok(None) => {
                                    if let Some(default) = &field.default {
                                        content.insert(field.name.clone(), default.clone());
                                    } else if field.required {
                                        screen(&format!("    “{}” is required.\n", field.title));
                                        continue;
                                    }
                                    break;
                                }
                                Ok(Some(value)) => match field.check(&value) {
                                    Ok(value) => {
                                        content.insert(field.name.clone(), value);
                                        break;
                                    }
                                    Err(problem) => screen(&format!("    {problem}\n")),
                                },
                                Err(problem) => screen(&format!("    {problem}\n")),
                            }
                        }
                    }
                    if ended {
                        shown.answered = true;
                        screen("\n    No more input: no answer.\n");
                        Elicited::Cancelled
                    } else {
                        match form.check(&content) {
                            Err(problem) => {
                                screen(&format!("    {problem}\n"));
                                Elicited::Cancelled
                            }
                            Ok(content) => match yes_or_no!("Send? [y/n] ") {
                                None => Elicited::Cancelled,
                                Some(true) => Elicited::Accepted(content),
                                Some(false) => Elicited::Declined,
                            },
                        }
                    }
                }
                _ => match yes_or_no!("[y/n] ") {
                    None => Elicited::Cancelled,
                    Some(true) => Elicited::Accepted(Map::new()),
                    Some(false) => Elicited::Declined,
                },
            };
            shown.answered = true;
            Ok(answer)
        })
    }
}

fn key_of(scope: Scope) -> char {
    match scope {
        Scope::Once => 'o',
        Scope::Session => 's',
        Scope::Approve => 'a',
    }
}

/// The keys a question takes: `[y/n]`, with `b` if it can block, or its scopes.
fn keys_of(choices: Option<&Choices>) -> String {
    let mut keys: Vec<char> = match choices {
        Some(c) if !c.scopes.is_empty() => c.scopes.iter().map(|(s, _)| key_of(*s)).collect(),
        _ => vec!['y'],
    };
    keys.push('n');
    if choices.is_some_and(|c| c.block.is_some()) {
        keys.push('b');
    }
    let keys: Vec<String> = keys.iter().map(char::to_string).collect();
    format!("[{}] ", keys.join("/"))
}

/// What a line answers, if anything: only what the question offers. A plain yes is the
/// first scope ("only this call").
fn answer(line: &str, choices: Option<&Choices>) -> Option<Reply> {
    let offers = |scope| choices.is_some_and(|c| c.scopes.iter().any(|(s, _)| *s == scope));
    let scope = |scope| Reply::Allow {
        scope,
        always: false,
    };
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => Some(Reply::YES),
        "n" | "no" => Some(Reply::Deny),
        "b" | "block" if choices.is_some_and(|c| c.block.is_some()) => Some(Reply::Block),
        "o" | "once" if offers(Scope::Once) => Some(scope(Scope::Once)),
        "s" | "session" if offers(Scope::Session) => Some(scope(Scope::Session)),
        "a" | "approve" if offers(Scope::Approve) => Some(scope(Scope::Approve)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clak_hub::{ConsentKind, ConsentLine};

    struct Terminal {
        prompt: Arc<ConsolePrompt>,
        keys: mpsc::UnboundedSender<String>,
        screen: Arc<std::sync::Mutex<String>>,
    }

    impl Terminal {
        fn new(timeout: Duration) -> Terminal {
            let (keys, lines) = mpsc::unbounded_channel();
            let screen = Arc::new(std::sync::Mutex::new(String::new()));
            let writer = screen.clone();
            let prompt = ConsolePrompt::with(
                lines,
                Arc::new(move |text: &str| writer.lock().unwrap().push_str(text)),
                timeout,
            );
            Terminal {
                prompt: Arc::new(prompt),
                keys,
                screen,
            }
        }

        fn ask(&self, title: &str, cancel: &CancellationToken) -> BoxFuture<Result<Reply, String>> {
            self.prompt.ask(request(title), cancel.clone())
        }

        fn type_line(&self, line: &str) {
            self.keys.send(format!("{line}\n")).unwrap();
        }

        fn text(&self) -> String {
            self.screen.lock().unwrap().clone()
        }

        /// Until the screen shows `what` `times` times.
        async fn shows(&self, what: &str, times: usize) {
            for _ in 0..400 {
                if self.text().matches(what).count() >= times {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("never showed {what:?} {times} times:\n{}", self.text());
        }
    }

    fn request(title: &str) -> ConsentRequest {
        ConsentRequest {
            asks: None,
            id: title.into(),
            kind: ConsentKind::Use,
            title: title.into(),
            question: "Allow it?".into(),
            who: vec![ConsentLine::new("Agent", "Claude Code")],
            what: vec![ConsentLine::new("Program", "notes")],
            tools: vec![],
            warning: None,
            note: "Until the agent exits.".into(),
            arguments: vec![],
            choices: None,
        }
    }

    const MINUTE: Duration = Duration::from_secs(60);

    fn server_question(title: &str, asks: Asks) -> ConsentRequest {
        ConsentRequest {
            asks: Some(asks),
            kind: ConsentKind::Confirm,
            question: "The server asks.".into(),
            note: "For this command only.".into(),
            ..request(title)
        }
    }

    #[tokio::test]
    async fn a_server_s_form_is_asked_one_field_at_a_time() {
        let terminal = Terminal::new(MINUTE);
        let form = clak_hub::Form::parse(Some(&json!({"properties": {
            "name": {"type": "string", "title": "Name"},
            "age": {"type": "integer", "minimum": 18},
            "color": {"type": "string", "enum": ["red", "blue"], "default": "red"},
            "news": {"type": "boolean"},
        }, "required": ["name", "age"]})))
        .unwrap();
        let answer = tokio::spawn(terminal.prompt.elicit(
            server_question("form", Asks::Form(form)),
            CancellationToken::new(),
        ));
        terminal.shows("  Name (text): ", 1).await;
        terminal.type_line("");
        terminal.shows("“Name” is required.", 1).await;
        terminal.type_line("Ada");
        terminal.shows("  age (a whole number): ", 1).await;
        terminal.type_line("17");
        terminal.shows("must be at least 18", 1).await;
        terminal.type_line("36");
        terminal
            .shows("      2. blue\n  color (a number, optional) [red]: ", 1)
            .await;
        terminal.type_line("2");
        terminal.shows("  news (y or n, optional): ", 1).await;
        terminal.type_line("");
        terminal.shows("Send? [y/n] ", 1).await;
        terminal.type_line("y");
        assert_eq!(
            answer.await.unwrap(),
            Ok(Elicited::Accepted(
                json!({"name": "Ada", "age": 36, "color": "blue"})
                    .as_object()
                    .unwrap()
                    .clone()
            ))
        );
        assert!(!terminal.text().contains("Withdrawn"));
    }

    #[tokio::test]
    async fn a_server_s_page_is_shown_whole_and_waits_until_the_user_is_done() {
        let terminal = Terminal::new(MINUTE);
        let page = clak_hub::Page::parse("https://example.com/connect?id=1", true).unwrap();
        let answer = tokio::spawn(terminal.prompt.elicit(
            server_question("page", Asks::Page(page)),
            CancellationToken::new(),
        ));
        terminal
            .shows(
                "    Address: https://example.com/connect?id=1\n    Site: example.com\n",
                1,
            )
            .await;
        terminal.type_line("y");
        terminal.shows("press Enter", 1).await;
        terminal.type_line("");
        assert_eq!(answer.await.unwrap(), Ok(Elicited::Accepted(Map::new())));

        // Declined: a no for the server.
        let page = clak_hub::Page::parse("https://example.com/", false).unwrap();
        let answer = tokio::spawn(terminal.prompt.elicit(
            server_question("again", Asks::Page(page)),
            CancellationToken::new(),
        ));
        terminal.shows("=== again", 1).await;
        terminal.type_line("n");
        assert_eq!(answer.await.unwrap(), Ok(Elicited::Declined));
    }

    #[tokio::test]
    async fn a_line_typed_before_the_question_does_not_answer_it() {
        let terminal = Terminal::new(MINUTE);
        let cancel = CancellationToken::new();
        terminal.type_line("y");
        let answer = tokio::spawn(terminal.ask("Claude Code wants notes", &cancel));
        terminal.shows("[y/n] ", 1).await;
        terminal.type_line("maybe");
        terminal.shows("[y/n] ", 2).await;
        terminal.type_line("n");
        assert_eq!(answer.await.unwrap(), Ok(Reply::Deny));
        let text = terminal.text();
        assert!(
            text.contains(
                "=== Claude Code wants notes\n    Agent: Claude Code\n    Program: notes\n"
            )
        );
        assert!(
            text.contains("    60 s to answer.\nAllow it? [y/n] "),
            "{text}"
        );
        assert!(
            !text.contains("Withdrawn") && !text.contains("Expired"),
            "{text}"
        );

        let answer = tokio::spawn(terminal.ask("again", &cancel));
        terminal.shows("=== again", 1).await;
        terminal.type_line(" YES ");
        assert_eq!(answer.await.unwrap(), Ok(Reply::YES));
    }

    #[tokio::test]
    async fn the_next_question_is_not_answered_by_a_line_for_the_one_withdrawn() {
        let terminal = Terminal::new(MINUTE);
        let first = CancellationToken::new();
        let second = CancellationToken::new();
        let answer = tokio::spawn(terminal.ask("first", &first));
        // Waiting for its turn.
        let next = tokio::spawn(terminal.ask("second", &second));
        terminal.shows("=== first", 1).await;
        first.cancel();
        assert_eq!(answer.await.unwrap(), Ok(Reply::Deny));
        terminal
            .shows("Withdrawn, whoever asked has gone: first.", 1)
            .await;
        terminal.shows("=== second", 1).await;
        // The second, on screen below the notice, takes the next line.
        terminal.type_line("n");
        assert_eq!(next.await.unwrap(), Ok(Reply::Deny));

        // The hub drops a question it no longer needs, without waiting for it: still said.
        let gone = CancellationToken::new();
        let question = terminal.ask("third", &gone);
        let shown = tokio::spawn(question);
        terminal.shows("=== third", 1).await;
        shown.abort();
        let _ = shown.await;
        terminal
            .shows("Withdrawn, whoever asked has gone: third.", 1)
            .await;

        // What was typed meanwhile doesn't answer the next one.
        terminal.type_line("y");
        let answer = tokio::spawn(terminal.ask("fourth", &CancellationToken::new()));
        terminal.shows("=== fourth", 1).await;
        terminal.type_line("n");
        assert_eq!(answer.await.unwrap(), Ok(Reply::Deny));
    }

    #[tokio::test]
    async fn a_question_whose_time_runs_out_says_it_expired() {
        let terminal = Terminal::new(Duration::from_millis(600));
        let limit = CancellationToken::new();
        let answer = tokio::spawn(terminal.ask("late", &limit));
        terminal.shows("=== late", 1).await;
        tokio::time::sleep(Duration::from_millis(650)).await;
        limit.cancel();
        assert_eq!(answer.await.unwrap(), Ok(Reply::Deny));
        terminal
            .shows("Expired, no answer within 1 s: late.", 1)
            .await;
        assert!(!terminal.text().contains("Withdrawn"));
    }

    #[tokio::test]
    async fn a_command_question_takes_how_far_the_yes_goes() {
        let terminal = Terminal::new(MINUTE);
        let mut asked = request("run");
        asked.kind = ConsentKind::Call;
        asked.arguments = vec![ConsentLine::new("text", "milk")];
        asked.choices = Some(Choices {
            scopes: vec![
                (Scope::Once, "Only this call".into()),
                (Scope::Session, "“read” for this agent's session".into()),
                (Scope::Approve, "Approve “read”, as it is, for notes".into()),
            ],
            always: None,
            block: Some("Block notes for Claude Code".into()),
        });
        let cancel = CancellationToken::new();
        let answer = tokio::spawn(terminal.prompt.ask(asked.clone(), cancel.clone()));
        terminal.shows("[o/s/a/n/b] ", 1).await;
        // An unknown key asks again.
        terminal.type_line("x");
        terminal.shows("[o/s/a/n/b] ", 2).await;
        terminal.type_line("s");
        assert_eq!(
            answer.await.unwrap(),
            Ok(Reply::Allow {
                scope: Scope::Session,
                always: false
            })
        );
        let text = terminal.text();
        assert!(text.contains("      text: milk\n"), "{text}");
        assert!(text.contains("b: Block notes for Claude Code"), "{text}");
        for (line, reply) in [
            ("y", Reply::YES),
            (
                "a",
                Reply::Allow {
                    scope: Scope::Approve,
                    always: false,
                },
            ),
            ("b", Reply::Block),
        ] {
            let before = terminal.text().matches("=== run").count();
            let answer = tokio::spawn(terminal.prompt.ask(asked.clone(), cancel.clone()));
            terminal.shows("=== run", before + 1).await;
            terminal.type_line(line);
            assert_eq!(answer.await.unwrap(), Ok(reply), "{line}");
        }
        // A plain question has no scopes, nor a block.
        assert_eq!(super::answer("s", None), None);
        assert_eq!(super::answer("b", None), None);
        assert_eq!(super::answer("y", None), Some(Reply::YES));
        assert_eq!(keys_of(None), "[y/n] ");
        // The question to use a program: yes, no, or block.
        let use_app = Choices {
            block: Some("Block".into()),
            ..Choices::default()
        };
        assert_eq!(keys_of(Some(&use_app)), "[y/n/b] ");
        assert_eq!(super::answer("b", Some(&use_app)), Some(Reply::Block));
    }

    #[tokio::test]
    async fn without_input_the_answer_is_no() {
        let Terminal {
            prompt,
            keys,
            screen,
        } = Terminal::new(MINUTE);
        drop(keys);
        let answer = prompt.ask(request("alone"), CancellationToken::new()).await;
        assert_eq!(answer, Ok(Reply::Deny));
        let text = screen.lock().unwrap().clone();
        assert!(text.ends_with("\n    No more input: no.\n"), "{text}");
        assert!(!text.contains("Withdrawn"));
    }
}
