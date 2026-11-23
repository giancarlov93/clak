//! The hub's questions, as windows. It is the only place where a permission is born.
//!
//! They appear in the corner near the tray (bottom right on Windows, top right below the
//! menu bar on the Mac), above the other windows, but without taking focus: an Enter
//! typed in another window must not be able to answer on the user's behalf.
//! For the same reason the buttons light up after a moment (the page does it), and neither
//! of them is the default. Closing it is a no. Several questions at once stack up, and each
//! says which it is of how many ("1 of 3").
//!
//! A question is built out of sight and appears once its page has taken its measure: before,
//! the window would show its empty frame (white, on Windows, while its WebView2 starts), then
//! move and change height under the user's eyes.
//!
//! A command's question says how far a yes goes: this call, the command for the agent's
//! session, or approved in Clak; the questions about a program can also block it for the
//! agent. The command itself is one row: what it does, what it takes and what the agent sends
//! open in a window of their own beside the question ([`WindowPrompt::show_command`]), which
//! goes with it. The yes to use a program holds for the agent's session only: nothing makes
//! it standing. When a program first connects, its commands can be approved with the yes.
//!
//! An MCP server's question halfway through a command (elicitation) can be a form: the user
//! clicks into the window to type, since it doesn't take the focus by itself, and what they
//! send is checked here against the form before it goes. Or it can be a page: the window
//! shows the address and opens it in the system's browser, never in a webview, only when
//! the user says so; for a command waiting for it, the window then waits with "Continue".

use clak_hub::consent::BoxFuture;
use clak_hub::{Asks, Choices, ConsentPrompt, ConsentRequest, Elicited, Reply};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const WIDTH: f64 = 500.0;
const GAP: f64 = 12.0;
/// The height a question has until its page says how tall it is.
const HEIGHT: f64 = 420.0;
/// If the page never says its height, the question appears anyway after this.
const FALLBACK: Duration = Duration::from_millis(1500);
/// The window with a command's question in full.
const COMMAND_WIDTH: f64 = 560.0;
const COMMAND_HEIGHT: f64 = 600.0;

struct Open {
    request: Value,
    /// What the question offers: an answer gets nothing more.
    choices: Option<Choices>,
    /// A server's question: its form, or its page.
    asks: Option<Asks>,
    expires: f64,
    answer: Option<Answer>,
    height: f64,
    /// On screen: until then it has no place in the stack.
    shown: bool,
    /// The page was opened, and the command waits for the user to finish there.
    opened: bool,
}

/// Where the answer goes: a permission's reply, or a server question's.
enum Answer {
    Reply(oneshot::Sender<Reply>),
    Elicited(oneshot::Sender<Elicited>),
}

#[derive(Default)]
struct Questions {
    open: HashMap<String, Open>,
    /// In order of arrival: the first sits closest to the corner.
    order: Vec<String>,
}

/// The questions open now, and who is showing them.
#[derive(Clone)]
pub struct WindowPrompt {
    app: AppHandle,
    questions: Arc<Mutex<Questions>>,
    timeout: std::time::Duration,
    sound: Arc<dyn Fn() -> bool + Send + Sync>,
}

pub fn label(id: &str) -> String {
    format!(
        "consent-{}",
        id.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    )
}

/// The window of a question's command, in full: never taken for a question's own.
fn command_label(id: &str) -> String {
    format!(
        "command-{}",
        id.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    )
}

impl WindowPrompt {
    pub fn new(
        app: AppHandle,
        timeout: std::time::Duration,
        sound: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        WindowPrompt {
            app,
            questions: Arc::default(),
            timeout,
            sound,
        }
    }

    fn questions(&self) -> std::sync::MutexGuard<'_, Questions> {
        self.questions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The question the page shows, with its deadline: the page asks for it as soon as it opens.
    pub fn request(&self, id: &str) -> Option<Value> {
        let questions = self.questions();
        let open = questions.open.get(id)?;
        let mut request = open.request.clone();
        request["expires"] = json!(open.expires);
        request["queue"] = queue(&questions, id);
        Some(request)
    }

    /// The user's answer. Only one: the others don't count. Only what the question offered:
    /// a scope, "always" or a block only in a command's question, "always" only where it can
    /// be made.
    pub fn answer(&self, id: &str, reply: Reply) {
        let sender = {
            let mut questions = self.questions();
            questions.open.get_mut(id).and_then(|open| {
                let reply = Choices::allowed(open.choices.as_ref(), reply);
                // A server's question answered as a plain yes or no: only a confirmation.
                let plain = matches!(&open.asks, Some(Asks::Form(f)) if f.fields.is_empty());
                match open.answer.take()? {
                    Answer::Reply(sender) => Some(Answer::Reply(sender)).zip(Some(reply)),
                    Answer::Elicited(sender) if plain || !reply.allows() => {
                        Some(Answer::Elicited(sender)).zip(Some(reply))
                    }
                    other => {
                        open.answer = Some(other);
                        None
                    }
                }
            })
        };
        match sender {
            Some((Answer::Reply(sender), reply)) => drop(sender.send(reply)),
            Some((Answer::Elicited(sender), reply)) => drop(sender.send(if reply.allows() {
                Elicited::Accepted(Map::new())
            } else {
                Elicited::Declined
            })),
            None => return,
        }
        self.close(id);
    }

    /// The answer to a server's question: `accept` with the form's content (checked against
    /// it: the error is for the user, and the window stays), or `decline`, or `cancel`. For a
    /// page, `accept` is "Continue", once it was opened.
    pub fn fill(&self, id: &str, action: &str, content: Map<String, Value>) -> Result<(), String> {
        let answered = {
            let mut questions = self.questions();
            let Some(open) = questions.open.get_mut(id) else {
                return Ok(());
            };
            let answer = match action {
                "accept" => match &open.asks {
                    Some(Asks::Form(form)) => Elicited::Accepted(form.check(&content)?),
                    Some(Asks::Page(_)) if open.opened => Elicited::Accepted(Map::new()),
                    Some(Asks::Page(_)) => return Err("Open the page first.".into()),
                    None => return Err("This question isn't a server's.".into()),
                },
                "decline" => Elicited::Declined,
                _ => Elicited::Cancelled,
            };
            match open.answer.take() {
                Some(Answer::Elicited(sender)) => Some((sender, answer)),
                other => {
                    open.answer = other;
                    None
                }
            }
        };
        if let Some((sender, answer)) = answered {
            let _ = sender.send(answer);
            self.close(id);
        }
        Ok(())
    }

    /// The user opens the page a server asks for: the question's own address, in the
    /// system's browser. True if the command then waits for the user to finish there; else
    /// the answer is given, and the window goes.
    pub fn open_page(&self, id: &str) -> Result<bool, String> {
        use tauri_plugin_opener::OpenerExt;
        let (url, wait) = {
            let questions = self.questions();
            match questions.open.get(id).map(|o| &o.asks) {
                Some(Some(Asks::Page(page))) => (page.url.clone(), page.wait),
                _ => return Err("This question has no page.".into()),
            }
        };
        self.app
            .opener()
            .open_url(&url, None::<&str>)
            .map_err(|e| format!("The browser didn't open: {e}"))?;
        if wait {
            if let Some(open) = self.questions().open.get_mut(id) {
                open.opened = true;
            }
            return Ok(true);
        }
        self.fill_opened(id);
        Ok(false)
    }

    /// A page opened, for a command that doesn't wait: the answer is "accepted".
    fn fill_opened(&self, id: &str) {
        let sender = self
            .questions()
            .open
            .get_mut(id)
            .and_then(|o| o.answer.take());
        if let Some(Answer::Elicited(sender)) = sender {
            let _ = sender.send(Elicited::Accepted(Map::new()));
        }
        self.close(id);
    }

    /// The command a question is about, in full, in a window of its own beside it: what it
    /// does, what it takes, what changed, what the agent sends. It answers nothing, and goes
    /// with the question. Asked again, the one already open comes to the front.
    pub fn show_command(&self, id: &str) -> Result<(), String> {
        let name = {
            let questions = self.questions();
            let open = questions
                .open
                .get(id)
                .ok_or("This question is no longer open.")?;
            open.request["tools"][0]["name"]
                .as_str()
                .unwrap_or("command")
                .to_owned()
        };
        if let Some(window) = self.app.get_webview_window(&command_label(id)) {
            let _ = window.set_focus();
            return Ok(());
        }
        let url = WebviewUrl::App(format!("command.html?id={id}").into());
        let builder = WebviewWindowBuilder::new(&self.app, command_label(id), url)
            .title(format!("Clak — {name}"))
            .inner_size(COMMAND_WIDTH, COMMAND_HEIGHT)
            .min_inner_size(420.0, 280.0)
            .maximizable(false)
            .minimizable(false)
            // Above the other windows, as its question: behind it, it would never be seen.
            .always_on_top(true);
        let builder = match self.beside(id) {
            Some((x, y)) => builder.position(x, y),
            None => builder.center(),
        };
        crate::look::window(builder)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Where the command's window goes: beside its question, away from the corner, inside the
    /// screen. Logical, as the builder takes it.
    fn beside(&self, id: &str) -> Option<(f64, f64)> {
        let question = self.app.get_webview_window(&label(id))?;
        let at = question.outer_position().ok()?;
        let monitor = question.current_monitor().ok().flatten()?;
        let scale = monitor.scale_factor();
        let area = monitor.work_area();
        let (left, top) = (area.position.x as f64, area.position.y as f64);
        let bottom = top + area.size.height as f64;
        let (width, height, gap) = (COMMAND_WIDTH * scale, COMMAND_HEIGHT * scale, GAP * scale);
        let x = (at.x as f64 - width - gap).max(left + gap);
        let y = (at.y as f64).min(bottom - height - gap).max(top + gap);
        Some((x / scale, y / scale))
    }

    /// The command's window goes with its question.
    fn close_command(&self, id: &str) {
        if let Some(window) = self.app.get_webview_window(&command_label(id)) {
            let _ = window.destroy();
        }
    }

    /// The page says how tall it is: the window adapts and the stack is rebuilt. The first
    /// time, the question appears.
    pub fn resize(&self, id: &str, height: f64) {
        let shown = {
            let mut questions = self.questions();
            let Some(open) = questions.open.get_mut(id) else {
                return;
            };
            let height = height.clamp(160.0, 900.0);
            // Each move of a window with a backdrop costs a frame: only when something changed.
            if open.shown && (open.height - height).abs() < 0.5 {
                return;
            }
            open.height = height;
            open.shown
        };
        if shown {
            self.stack();
        } else {
            self.reveal(id);
        }
    }

    /// The question appears, once: when its page has taken its measure, or after the fallback.
    /// The page hears it, and its buttons count from then.
    fn reveal(&self, id: &str) {
        {
            let mut questions = self.questions();
            match questions.open.get_mut(id) {
                Some(open) if !open.shown => open.shown = true,
                _ => return,
            }
        }
        self.stack();
        let Some(window) = self.app.get_webview_window(&label(id)) else {
            return;
        };
        crate::look::appear(&window);
        let _ = window.emit_to(label(id), "consent-shown", id);
        if (self.sound)() {
            let _ = self
                .app
                .run_on_main_thread(|| crate::look::play(crate::look::Sound::Consent));
        }
    }

    fn close(&self, id: &str) {
        {
            let mut questions = self.questions();
            questions.open.remove(id);
            questions.order.retain(|o| o != id);
        }
        if let Some(window) = self.app.get_webview_window(&label(id)) {
            let _ = window.destroy();
        }
        self.close_command(id);
        self.stack();
    }

    /// Closed with the X (or by the system): it is a no, unless there was already an answer.
    pub fn closed_window(&self, window: &str) {
        let (id, sender) = {
            let mut questions = self.questions();
            let Some(id) = questions
                .order
                .iter()
                .find(|id| label(id) == window)
                .cloned()
            else {
                return;
            };
            questions.order.retain(|o| *o != id);
            let sender = questions.open.remove(&id).and_then(|mut o| o.answer.take());
            (id, sender)
        };
        match sender {
            Some(Answer::Reply(sender)) => drop(sender.send(Reply::Deny)),
            // A server's question closed without choosing: "cancel".
            Some(Answer::Elicited(sender)) => drop(sender.send(Elicited::Cancelled)),
            None => {}
        }
        self.close_command(&id);
        self.stack();
    }

    /// From the corner near the tray, one above (or below) the other; each told where it is
    /// in the line. Only those on screen take a place.
    fn stack(&self) {
        {
            let questions = self.questions();
            for id in &questions.order {
                if let Some(window) = self.app.get_webview_window(&label(id)) {
                    let _ = window.emit_to(label(id), "consent-queue", queue(&questions, id));
                }
            }
        }
        let (order, heights): (Vec<String>, Vec<f64>) = {
            let questions = self.questions();
            questions
                .order
                .iter()
                .filter_map(|id| {
                    let open = questions.open.get(id).filter(|o| o.shown)?;
                    Some((id.clone(), open.height))
                })
                .unzip()
        };
        let Some(first) = order
            .first()
            .and_then(|id| self.app.get_webview_window(&label(id)))
        else {
            return;
        };
        let Ok(Some(monitor)) = first.primary_monitor() else {
            return;
        };
        let scale = monitor.scale_factor();
        let area = monitor.work_area();
        let (left, top, right, bottom) = (
            area.position.x as f64,
            area.position.y as f64,
            area.position.x as f64 + area.size.width as f64,
            area.position.y as f64 + area.size.height as f64,
        );
        let width = WIDTH * scale;
        let gap = GAP * scale;
        let mut edge = if cfg!(target_os = "macos") {
            top + gap
        } else {
            bottom - gap
        };
        for (id, height) in order.iter().zip(heights) {
            let Some(window) = self.app.get_webview_window(&label(id)) else {
                continue;
            };
            // Never taller than the screen: the page scrolls, its buttons stay in sight. Whole
            // pixels, rounded up: a fraction short, the page would scroll. Logical: a window
            // that arrives from a screen of another scale keeps its size in points.
            let height = (height * scale).ceil().min((bottom - top) - 2.0 * gap);
            let _ = window.set_size(LogicalSize::new(WIDTH, height / scale));
            let y = if cfg!(target_os = "macos") {
                let y = edge;
                edge += height + gap;
                y
            } else {
                let y = (edge - height).max(top + gap);
                edge = y - gap;
                y
            };
            let x = (right - width - gap).max(left);
            let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
        }
    }

    fn show(&self, id: &str) -> Result<(), String> {
        let kind = self
            .request(id)
            .and_then(|r| r["kind"].as_str().map(str::to_owned));
        let request = self.request(id).unwrap_or_default();
        let title = match kind.as_deref() {
            Some("confirm") if !request["page"].is_null() => "Clak — page to open",
            Some("confirm")
                if request["form"]["fields"]
                    .as_array()
                    .is_some_and(|f| !f.is_empty()) =>
            {
                "Clak — information requested"
            }
            Some("confirm") => "Clak — confirmation",
            Some("change") => "Clak — settings",
            _ => "Clak — permission",
        };
        let url = WebviewUrl::App(format!("consent.html?id={id}").into());
        let builder = WebviewWindowBuilder::new(&self.app, label(id), url)
            .title(title)
            .inner_size(WIDTH, HEIGHT)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .always_on_top(true)
            .skip_taskbar(false)
            .focused(false);
        let builder = crate::look::unseen(crate::look::transient(builder), &self.app);
        builder.build().map_err(|e| e.to_string())?;
        // The other questions learn how many there are now.
        self.stack();
        let this = self.clone();
        let id = id.to_owned();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(FALLBACK).await;
            this.reveal(&id);
        });
        Ok(())
    }
}

/// Which question it is, of how many open: `{ "id", "position": 1, "total": 3 }`. The id
/// lets each window take only its own.
fn queue(questions: &Questions, id: &str) -> Value {
    let position = questions
        .order
        .iter()
        .position(|o| o == id)
        .map_or(0, |p| p + 1);
    json!({ "id": id, "position": position, "total": questions.order.len() })
}

impl WindowPrompt {
    /// A question in a window of its own, until its answer arrives or it is withdrawn.
    fn open(&self, request: ConsentRequest, answer: Answer) -> Result<String, String> {
        let id = request.id.clone();
        {
            let mut questions = self.questions();
            questions.open.insert(
                id.clone(),
                Open {
                    request: request.to_json(),
                    choices: request.choices.clone(),
                    asks: request.asks.clone(),
                    expires: clak_wire::json::now() + self.timeout.as_secs_f64(),
                    answer: Some(answer),
                    height: HEIGHT,
                    shown: false,
                    opened: false,
                },
            );
            questions.order.push(id.clone());
        }
        if let Err(problem) = self.show(&id) {
            self.close(&id);
            return Err(problem);
        }
        Ok(id)
    }
}

impl ConsentPrompt for WindowPrompt {
    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<Reply, String>> {
        let this = self.clone();
        Box::pin(async move {
            let (sender, receiver) = oneshot::channel();
            let id = this.open(request, Answer::Reply(sender))?;
            tokio::select! {
                answer = receiver => Ok(answer.unwrap_or(Reply::Deny)),
                _ = cancel.cancelled() => {
                    // Whoever was asking is no longer waiting, or another answer covered it:
                    // the question vanishes without an answer.
                    this.close(&id);
                    Ok(Reply::Deny)
                }
            }
        })
    }

    fn elicit(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<Elicited, String>> {
        let this = self.clone();
        Box::pin(async move {
            let (sender, receiver) = oneshot::channel();
            let id = this.open(request, Answer::Elicited(sender))?;
            tokio::select! {
                answer = receiver => Ok(answer.unwrap_or(Elicited::Cancelled)),
                _ = cancel.cancelled() => {
                    this.close(&id);
                    Ok(Elicited::Cancelled)
                }
            }
        })
    }
}
