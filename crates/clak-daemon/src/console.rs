//! The hub's questions on the terminal, for `--ask`: one at a time, answered "y" or "n" on a
//! line.
//!
//! Only a line typed while a question is on screen answers it. What was typed before (for a
//! question that has gone, or for none) is thrown away when the next one appears: otherwise a
//! "y" meant for one question would answer the next, unseen. A question that goes away
//! unanswered says so, and why: withdrawn, because whoever asked has gone, or expired,
//! because its time ran out.

use clak_hub::consent::BoxFuture;
use clak_hub::{ConsentPrompt, ConsentRequest};
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

impl ConsentPrompt for ConsolePrompt {
    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<bool, String>> {
        let lines = self.lines.clone();
        let screen = self.screen.clone();
        let timeout = self.timeout;
        let asked = Instant::now();
        Box::pin(async move {
            // One question at a time: whoever holds the receiver is the one on screen.
            let mut lines = tokio::select! {
                lines = lines.lock() => lines,
                _ = cancel.cancelled() => return Ok(false),
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
            }
            if let Some(warning) = &request.warning {
                text.push_str(&format!("    WARNING: {warning}\n"));
            }
            let left = seconds(timeout.saturating_sub(asked.elapsed()));
            text.push_str(&format!(
                "    {}\n    {left} s to answer.\n{} [y/n] ",
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
                    _ = cancel.cancelled() => return Ok(false),
                };
                // No more input: nobody can say yes.
                let Some(line) = line else {
                    shown.answered = true;
                    screen("\n    No more input: no.\n");
                    return Ok(false);
                };
                match line.trim().to_lowercase().as_str() {
                    "y" | "yes" => {
                        shown.answered = true;
                        return Ok(true);
                    }
                    "n" | "no" => {
                        shown.answered = true;
                        return Ok(false);
                    }
                    _ => screen("[y/n] "),
                }
            }
        })
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

        fn ask(&self, title: &str, cancel: &CancellationToken) -> BoxFuture<Result<bool, String>> {
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
            id: title.into(),
            kind: ConsentKind::Use,
            title: title.into(),
            question: "Allow it?".into(),
            who: vec![ConsentLine::new("Agent", "Claude Code")],
            what: vec![ConsentLine::new("Program", "notes")],
            tools: vec![],
            warning: None,
            note: "Until the agent exits.".into(),
            always: None,
        }
    }

    const MINUTE: Duration = Duration::from_secs(60);

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
        assert_eq!(answer.await.unwrap(), Ok(false));
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
        assert_eq!(answer.await.unwrap(), Ok(true));
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
        assert_eq!(answer.await.unwrap(), Ok(false));
        terminal
            .shows("Withdrawn, whoever asked has gone: first.", 1)
            .await;
        terminal.shows("=== second", 1).await;
        // The second, on screen below the notice, takes the next line.
        terminal.type_line("n");
        assert_eq!(next.await.unwrap(), Ok(false));

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
        assert_eq!(answer.await.unwrap(), Ok(false));
    }

    #[tokio::test]
    async fn a_question_whose_time_runs_out_says_it_expired() {
        let terminal = Terminal::new(Duration::from_millis(600));
        let limit = CancellationToken::new();
        let answer = tokio::spawn(terminal.ask("late", &limit));
        terminal.shows("=== late", 1).await;
        tokio::time::sleep(Duration::from_millis(650)).await;
        limit.cancel();
        assert_eq!(answer.await.unwrap(), Ok(false));
        terminal
            .shows("Expired, no answer within 1 s: late.", 1)
            .await;
        assert!(!terminal.text().contains("Withdrawn"));
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
        assert_eq!(answer, Ok(false));
        let text = screen.lock().unwrap().clone();
        assert!(text.ends_with("\n    No more input: no.\n"), "{text}");
        assert!(!text.contains("Withdrawn"));
    }
}
