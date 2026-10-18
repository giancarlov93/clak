//! The hub's questions, as windows. It is the only place where a permission is born.
//!
//! They appear in the corner near the tray (bottom right on Windows, top right below the
//! menu bar on the Mac), above the other windows, but without taking focus: an Enter
//! typed in another window must not be able to answer on the user's behalf.
//! For the same reason the buttons light up after a moment (the page does it), and neither
//! of them is the default. Closing it is a no. Several questions at once stack up.

use clak_hub::consent::BoxFuture;
use clak_hub::{ConsentPrompt, ConsentRequest};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const WIDTH: f64 = 500.0;
const GAP: f64 = 12.0;

struct Open {
    request: Value,
    expires: f64,
    answer: Option<oneshot::Sender<bool>>,
    height: f64,
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
        Some(request)
    }

    /// The user's answer. Only one: the others don't count.
    pub fn answer(&self, id: &str, allow: bool) {
        let sender = self
            .questions()
            .open
            .get_mut(id)
            .and_then(|o| o.answer.take());
        if let Some(sender) = sender {
            let _ = sender.send(allow);
        }
        self.close(id);
    }

    /// The page says how tall it is: the window adapts and the stack is rebuilt.
    pub fn resize(&self, id: &str, height: f64) {
        if let Some(open) = self.questions().open.get_mut(id) {
            open.height = height.clamp(160.0, 900.0);
        }
        self.stack();
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
        self.stack();
    }

    /// Closed with the X (or by the system): it is a no, unless there was already an answer.
    pub fn closed_window(&self, window: &str) {
        let sender = {
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
            questions.open.remove(&id).and_then(|mut o| o.answer.take())
        };
        if let Some(sender) = sender {
            let _ = sender.send(false);
        }
        self.stack();
    }

    /// From the corner near the tray, one above (or below) the other.
    fn stack(&self) {
        let (order, heights): (Vec<String>, Vec<f64>) = {
            let questions = self.questions();
            questions
                .order
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        questions.open.get(id).map_or(420.0, |o| o.height),
                    )
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
            let height = height * scale;
            let _ = window.set_size(PhysicalSize::new(width as u32, height as u32));
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
        let title = match kind.as_deref() {
            Some("confirm") => "Clak — confirmation",
            Some("change") => "Clak — settings",
            _ => "Clak — permission",
        };
        let url = WebviewUrl::App(format!("consent.html?id={id}").into());
        let builder = WebviewWindowBuilder::new(&self.app, label(id), url)
            .title(title)
            .inner_size(WIDTH, 420.0)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .always_on_top(true)
            .skip_taskbar(false)
            .focused(false)
            .visible(true);
        let builder = crate::look::transient(builder);
        builder.build().map_err(|e| e.to_string())?;
        self.stack();
        if (self.sound)() {
            crate::look::announce();
        }
        Ok(())
    }
}

impl ConsentPrompt for WindowPrompt {
    fn ask(
        &self,
        request: ConsentRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<bool, String>> {
        let this = self.clone();
        Box::pin(async move {
            let id = request.id.clone();
            let (sender, receiver) = oneshot::channel();
            {
                let mut questions = this.questions();
                questions.open.insert(
                    id.clone(),
                    Open {
                        request: request.to_json(),
                        expires: clak_wire::json::now() + this.timeout.as_secs_f64(),
                        answer: Some(sender),
                        height: 420.0,
                    },
                );
                questions.order.push(id.clone());
            }
            if let Err(problem) = this.show(&id) {
                this.close(&id);
                return Err(problem);
            }
            tokio::select! {
                answer = receiver => Ok(answer.unwrap_or(false)),
                _ = cancel.cancelled() => {
                    // Whoever was asking is no longer waiting: the question vanishes without an answer.
                    this.close(&id);
                    Ok(false)
                }
            }
        })
    }
}
