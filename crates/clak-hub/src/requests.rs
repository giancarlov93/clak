//! What an agent asks the hub, from the command line or the MCP shim: one request per
//! connection, and one response.
//!
//! ```text
//!     status                         who I am to the hub, without asking the user anything
//!     list                           the connected programs
//!     tools  {resource, query?}      a program's commands
//!     call   {resource, tool, arguments}
//! ```
//!
//! Every missing permission is asked of the user while the request waits. The caller knows
//! it: `progress` lines arrive before the response («waiting for the user…», «permission
//! granted…»), so an agent knows what it is waiting for.

use crate::commands::Status;
use crate::consent::{AgentIdentity, Reply, Scope, Verdict};
use crate::core::{Asked, Broker, CallTarget, Core, Gone, Progress, outcome};
use crate::wire::{Malformed, ToolDef};
use crate::{flags, texts};
use clak_wire::json::{self};
use clak_wire::names;
use serde_json::{Map, Value, json};
use std::sync::{Arc, OnceLock};
use tokio_util::sync::CancellationToken;

/// The response to a request (without `type`, which the channel adds), or [`Gone`] if the
/// requester has gone away and there is nobody to answer.
pub async fn handle(
    broker: &Broker,
    agent: Option<AgentIdentity>,
    request: &Map<String, Value>,
    gone: &CancellationToken,
    progress: Progress,
) -> Result<Map<String, Value>, Gone> {
    let Some(agent) = agent else {
        return Ok(error(
            "unknown_agent",
            "Cannot tell which agent launched this command, so I cannot ask for permission on its behalf. \
             This happens when between the agent and clak there is a process whose parent has already exited, for example \
             `timeout` or `xargs` in Git Bash: run clak directly (for time limits use --wait). It also happens with an agent \
             Clak does not recognize yet, started by a launcher that exits right away.",
        ));
    };
    let cwd = request
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let via = if request.get("via").and_then(Value::as_str) == Some("mcp") {
        "mcp"
    } else {
        "cli"
    };
    let token = request
        .get("launch")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty());
    redeem(broker, &agent, token, cwd.as_deref(), via).await;
    let context = Context {
        broker,
        agent: &agent,
        cwd: cwd.as_deref(),
        via,
        tell: progress,
        gone,
    };
    let result = match request.get("op").and_then(Value::as_str) {
        Some("status") => Ok(Ok(status(&context))),
        Some("list") => list(&context).await.map(Ok),
        Some("tools") => tools(&context, request).await.map(Ok),
        Some("call") => call(&context, request).await,
        op => Ok(Ok(error(
            "bad_request",
            &format!("unknown operation: {}", op.unwrap_or("none")),
        ))),
    }?;
    Ok(result.unwrap_or_else(|e: Malformed| {
        error("bad_request", &format!("malformed request: {}", e.text()))
    }))
}

/// An agent launched from Clak: authorized here, without a question, with its preset (see
/// [`crate::launch`]). The processes above it are read only if an app's launch is waiting.
async fn redeem(
    broker: &Broker,
    agent: &AgentIdentity,
    token: Option<&str>,
    cwd: Option<&str>,
    via: &str,
) {
    let (known, waiting, app) = broker.run(|c| {
        (
            c.session(agent).is_some(),
            c.awaits_launch(),
            c.awaits_app(),
        )
    });
    if known || !waiting || (token.is_none() && !app) {
        return;
    }
    let above = if app {
        let platform = broker.options().platform.clone();
        let pid = agent.pid;
        tokio::task::spawn_blocking(move || platform.above(pid))
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    broker.run(|c| c.redeem(agent, token, &above, cwd, via));
}

struct Context<'a> {
    broker: &'a Broker,
    agent: &'a AgentIdentity,
    cwd: Option<&'a str>,
    via: &'static str,
    tell: Progress,
    gone: &'a CancellationToken,
}

fn ok(body: Map<String, Value>) -> Map<String, Value> {
    let mut body = body;
    body.insert("ok".into(), Value::Bool(true));
    body
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

pub fn error(code: &str, text: &str) -> Map<String, Value> {
    object(json!({ "ok": false, "code": code, "error": text }))
}

/// The question is on the hub's web page: the agent passes the link on, and asks again.
fn pending(link: &str, what: &str) -> Map<String, Value> {
    error(
        "consent_pending",
        &format!(
            "To {what} the user's consent is required. Ask the user to open {link} and answer there, then try again: until they answer, trying again gives this same link."
        ),
    )
}

/// What the agent is told while it waits: nothing, when it doesn't wait (the questions are
/// on the web page, and the link comes in the response).
fn waiting(cx: &Context, what: &str) {
    if cx.broker.options().question_link.is_none() {
        cx.tell.waiting(what);
    }
}

/// The user blocked the program for this agent: no question, until its process ends.
fn blocked(app_name: &str) -> Map<String, Value> {
    error(
        "blocked",
        &format!(
            "The user blocked '{app_name}' for this agent: it can't use it until this agent's process ends. Do not retry; tell the user if you need it."
        ),
    )
}

fn refused(verdict: Verdict, what: &str) -> Map<String, Value> {
    match verdict {
        Verdict::Denied => error(
            "denied",
            &format!(
                "The user declined the request to {what}. Do not retry until they ask you to."
            ),
        ),
        Verdict::NoPrompt => error(
            "no_prompt",
            &format!(
                "To {what} the user's consent is required, but this hub has no interface to ask for it: the Clak app must be started."
            ),
        ),
        _ => error(
            "no_answer",
            &format!(
                "Nobody answered the request to {what}: the question appeared in a Clak window. Ask the user to look at it and try again."
            ),
        ),
    }
}

fn status(cx: &Context) -> Map<String, Value> {
    let (authorized, resources) = cx
        .broker
        .run(|c| (c.session(cx.agent).is_some(), c.app_count()));
    ok(object(json!({
        "agent": { "name": cx.agent.name, "exe": cx.agent.exe, "pid": cx.agent.pid },
        "authorized": authorized,
        "resources": if authorized { json!(resources) } else { Value::Null },
    })))
}

/// The connected agent (asking the user if needed): its key, or the response to give.
async fn session(cx: &Context<'_>) -> Result<Result<String, Map<String, Value>>, Gone> {
    let key = cx.agent.key();
    if cx.broker.run(|c| c.session(cx.agent).is_some()) {
        return Ok(Ok(key));
    }
    waiting(cx, "whether this agent may connect");
    let (agent, cwd, via) = (cx.agent.clone(), cx.cwd.map(str::to_owned), cx.via);
    let verdict = match cx
        .broker
        .agent_consent(
            &format!("session:{key}"),
            |id, _| texts::session(id, cx.agent, cx.cwd),
            move |c, reply| {
                if reply.allows() {
                    c.authorize(&agent, cwd.as_deref(), via);
                }
            },
            None,
            cx.gone,
        )
        .await?
    {
        Asked::Verdict(verdict) => verdict,
        Asked::Link(link) => return Ok(Err(pending(&link, "connect to Clak"))),
    };
    if verdict != Verdict::Allowed {
        return Ok(Err(refused(verdict, "connect to Clak")));
    }
    cx.tell.allowed("permission granted.");
    Ok(if cx.broker.run(|c| c.session(cx.agent).is_some()) {
        Ok(key)
    } else {
        Err(error(
            "gone",
            "Permission was granted and immediately revoked: try again.",
        ))
    })
}

async fn list(cx: &Context<'_>) -> Result<Map<String, Value>, Gone> {
    let key = match session(cx).await? {
        Ok(key) => key,
        Err(answer) => return Ok(answer),
    };
    Ok(ok(object(json!({
        "resources": cx.broker.run(|c| c.listing(&key)),
    }))))
}

/// A program that is not there. The closest name is suggested only to an already connected
/// agent: the others do not even see the list.
fn not_found(broker: &Broker, name: Option<&str>, suggest: bool) -> Map<String, Value> {
    let near = if suggest {
        names::closest(name, broker.run(|c| c.app_names()))
    } else {
        None
    };
    error(
        "not_found",
        &format!(
            "No connected program is named '{}'{} `clak list --all` shows the connected ones.",
            name.unwrap_or(""),
            near.map_or(".".to_owned(), |n| format!(": did you mean '{n}'?"))
        ),
    )
}

/// A command that is not there. The closest name is suggested, never one the user turned
/// off.
fn no_command(c: &Core, app: &crate::core::AppConn, name: Option<&str>) -> Map<String, Value> {
    let app_name = &app.name;
    let near = names::closest(
        name,
        app.tools
            .iter()
            .filter(|t| c.status_of(app, t) != Status::Off)
            .map(|t| t.name.as_str()),
    );
    error(
        "not_found",
        &format!(
            "'{app_name}' has no command '{}'{} `clak {app_name} list` shows the available ones.",
            name.unwrap_or(""),
            near.map_or(".".to_owned(), |n| format!(": did you mean '{n}'?"))
        ),
    )
}

async fn tools(cx: &Context<'_>, request: &Map<String, Value>) -> Result<Map<String, Value>, Gone> {
    let key = match session(cx).await? {
        Ok(key) => key,
        Err(answer) => return Ok(answer),
    };
    let name = request.get("resource").and_then(Value::as_str);
    let query = request.get("query").and_then(Value::as_str);
    let described = cx.broker.run(|c| {
        let app = c.app(name?)?;
        let conn_id = app.conn_id.clone();
        c.describe(&key, &conn_id, query)
    });
    Ok(match described {
        Some(body) => ok(body),
        None => not_found(cx.broker, name, true),
    })
}

async fn call(
    cx: &Context<'_>,
    request: &Map<String, Value>,
) -> Result<Result<Map<String, Value>, Malformed>, Gone> {
    let name = request.get("resource").and_then(Value::as_str);
    let tool_name = request.get("tool").and_then(Value::as_str);
    if !matches!(
        request.get("arguments"),
        None | Some(Value::Null) | Some(Value::Object(_))
    ) {
        return Ok(Ok(error("bad_request", "arguments must be a JSON object")));
    }
    if !matches!(
        request.get("flags"),
        None | Some(Value::Null) | Some(Value::Array(_))
    ) {
        return Ok(Ok(error("bad_request", "'flags' must be a list")));
    }
    let key = cx.agent.key();
    let Some(app) = cx.broker.run(|c| name.and_then(|n| c.app(n)).cloned()) else {
        let suggest = cx.broker.run(|c| c.session(cx.agent).is_some());
        return Ok(Ok(not_found(cx.broker, name, suggest)));
    };

    // Before bothering the user: does the command exist, and do the parameters add up?
    let (conn_id, app_name) = (app.conn_id.clone(), app.name.clone());
    let Some(asked) = app
        .tools
        .iter()
        .find(|t| Some(t.name.as_str()) == tool_name)
    else {
        return Ok(Ok(cx.broker.run(|c| no_command(c, &app, tool_name))));
    };
    let given = request.get("arguments").and_then(Value::as_object).cloned();
    let flags_given = match request.get("flags") {
        Some(Value::Array(flags)) => Some(flags.clone()),
        _ => None,
    };
    let mut arguments = given.clone();
    if let Some(flags) = &flags_given {
        match flags::apply(asked, arguments.as_ref(), flags) {
            Ok(applied) => arguments = Some(applied),
            Err(problem) => return Ok(Ok(error("bad_request", &problem))),
        }
    }
    let asked_digest = asked.digest.clone();

    // A program the user blocked for this agent: refused, without a question.
    if cx.broker.run(|c| c.blocks(&key, &app)) {
        return Ok(Ok(blocked(&app_name)));
    }
    let mut waited = false;

    // First, may the agent use the program at all? Asked the first time in each of its
    // sessions, even when all its commands are approved: approving a command doesn't choose
    // who uses it. Only a preset the user launched the agent with lets it in without asking.
    let mut access = cx.broker.run(|c| c.may_use(&key, &app));
    if access.is_none() {
        let asked = if app.is_hub() {
            use_settings(cx, &app).await?
        } else {
            ask_use(cx, &app).await?
        };
        match asked {
            Ok(()) => waited = true,
            Err(answer) => return Ok(Ok(answer)),
        }
        access = cx.broker.run(|c| c.may_use(&key, &app));
    }
    // A preset: its permission on this instance, without a question.
    let entered = cx.broker.run(|c| c.enter(&key, &conn_id));
    let (Some(access), Some(_)) = (access, entered) else {
        return Ok(Ok(error(
            "gone",
            "The permission changed in the meantime: try again.",
        )));
    };

    // Then the command: it runs if the user approved it as it is (and the agent's preset
    // doesn't leave it out), or allowed it for the agent's session; otherwise this call
    // asks. The same call (agent, program, command and arguments) asked twice waits for the
    // same answer; "only this call" lets one of them run, the others ask again.
    let arguments_digest = json::digest(&Value::Object(arguments.clone().unwrap_or_default()));
    /// One look at the call, under one lock: what to do next.
    enum Step {
        Begun(String, tokio::sync::oneshot::Receiver<Value>, ToolDef),
        Ask(Box<crate::core::AppConn>, ToolDef, Status, bool, String),
        Answer(Map<String, Value>),
    }
    let (routed, answer, tool) = loop {
        let step = cx.broker.run(|c| {
            let Some(app) = c.app_by_id(&conn_id).cloned() else {
                return Step::Answer(error("gone", &format!("'{app_name}' was closed.")));
            };
            if c.blocks(&key, &app) {
                return Step::Answer(blocked(&app_name));
            }
            let Some(tool) = app
                .tools
                .iter()
                .find(|t| Some(t.name.as_str()) == tool_name)
                .cloned()
            else {
                return Step::Answer(no_command(c, &app, tool_name));
            };
            let status = c.status_of(&app, &tool);
            if status == Status::Off {
                return Step::Answer(error(
                    "off",
                    &format!(
                        "The user turned off '{app_name} {}': it can't be run. Do not retry; tell the user if you need it.",
                        tool.name
                    ),
                ));
            }
            let restricted = access.restricts(&tool.name);
            let call_key = format!(
                "call:{key}:{conn_id}:{}:{}:{arguments_digest}",
                tool.name, tool.digest
            );
            let Some(grant_id) = c.grant_of(&key, &conn_id).map(|g| g.id.clone()) else {
                return Step::Answer(error(
                    "gone",
                    "The permission was revoked in the meantime: ask again.",
                ));
            };
            if c.runs(&key, &conn_id, &tool, restricted) || c.take_permit(&call_key) {
                // The command changed while the user decided: the parameters given on the
                // command line are read again with its schema now.
                let sent = match &flags_given {
                    Some(flags) if tool.digest != asked_digest => {
                        match flags::apply(&tool, given.as_ref(), flags) {
                            Ok(applied) => Some(applied),
                            Err(problem) => return Step::Answer(error("bad_request", &problem)),
                        }
                    }
                    _ => arguments.take(),
                };
                return match c.begin_call(&grant_id, &tool, sent, cx.tell.clone()) {
                    Some((routed, answer)) => Step::Begun(routed, answer, tool),
                    None => Step::Answer(error(
                        "gone",
                        "The permission or the command changed in the meantime: try again.",
                    )),
                };
            }
            Step::Ask(Box::new(app), tool, status, restricted, call_key)
        });
        let (app, tool, status, restricted, call_key) = match step {
            Step::Begun(routed, answer, tool) => break (routed, answer, tool),
            Step::Answer(answer) => return Ok(Ok(answer)),
            Step::Ask(app, tool, status, restricted, call_key) => {
                (app, tool, status, restricted, call_key)
            }
        };
        let call = CallAsk {
            key: &call_key,
            app: &app,
            tool: &tool,
            arguments: arguments.as_ref(),
            status,
            restricted,
        };
        match ask_call(cx, &call).await? {
            Ok(()) => waited = true,
            Err(answer) => return Ok(Ok(answer)),
        }
    };
    if waited {
        cx.tell.allowed(&format!(
            "permission granted: '{app_name} {}' has started.",
            tool.name
        ));
    }

    let limit = cx.broker.options().call_timeout;
    let result = tokio::select! {
        result = tokio::time::timeout(limit, answer) => result,
        _ = cx.gone.cancelled() => {
            cx.broker.run(|c| c.abandon(&routed, "the requester went away", outcome::INTERRUPTED));
            return Err(Gone);
        }
    };
    let result = match result {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => crate::core::failure("the program was closed during the command"),
        Err(_) => {
            cx.broker
                .run(|c| c.abandon(&routed, "timed out", outcome::TIMEOUT));
            let seconds = format_seconds(limit.as_secs_f64());
            return Ok(Ok(error(
                "app_timeout",
                &format!("'{app_name}' did not answer within {seconds} s."),
            )));
        }
    };
    Ok(Ok(ok(object(json!({
        "is_error": json::truthy(result.get("is_error")),
        "content": result.get("content").cloned().unwrap_or(Value::Null),
    })))))
}

/// Clak's settings, once for the agent's session: the question that lets it see them and
/// propose changes. Every change asks anyway. Ok when the agent may go on.
async fn use_settings(
    cx: &Context<'_>,
    app: &crate::core::AppConn,
) -> Result<Result<(), Map<String, Value>>, Gone> {
    let key = cx.agent.key();
    let conn_id = app.conn_id.clone();
    let new_session = cx.broker.run(|c| c.session(cx.agent).is_none());
    waiting(
        cx,
        &if new_session {
            "whether this agent may connect and use Clak's settings".to_owned()
        } else {
            "for permission to use Clak's settings".to_owned()
        },
    );
    // The commands the question shows, as they are when it is asked: the yes allows those.
    // The question is built only for the first agent that asks, and only its answer is
    // kept: the two halves always belong together.
    let shown: Arc<OnceLock<Vec<(String, String)>>> = Arc::default();
    let shown_then = shown.clone();
    let (agent, cwd, via, target) = (
        cx.agent.clone(),
        cx.cwd.map(str::to_owned),
        cx.via,
        conn_id.clone(),
    );
    let verdict = cx
        .broker
        .agent_consent(
            &format!("use:{key}:{conn_id}"),
            |id, c| {
                let app = c.app_by_id(&conn_id).unwrap_or(app);
                let _ = shown.set(
                    app.tools
                        .iter()
                        .map(|t| (t.name.clone(), t.digest.clone()))
                        .collect(),
                );
                texts::use_settings(id, cx.agent, cx.cwd, new_session, app)
            },
            move |c, reply| {
                if reply.allows() && c.registered(&target) {
                    let session = c.allow_use(&agent, cwd.as_deref(), via, &target);
                    let shown = shown_then.get().map_or(&[][..], Vec::as_slice);
                    if let Some(session) = session {
                        c.allow_for_session(&session, &target, shown);
                    }
                }
            },
            None,
            cx.gone,
        )
        .await?;
    let what = "use Clak's settings";
    Ok(match verdict {
        Asked::Verdict(Verdict::Allowed) => Ok(()),
        Asked::Verdict(verdict) => Err(refused(verdict, what)),
        Asked::Link(link) => Err(pending(&link, what)),
    })
}

/// May the agent use this program? Asked the first time it does, for its session: all the
/// instances of the application. Ok when it may.
async fn ask_use(
    cx: &Context<'_>,
    app: &crate::core::AppConn,
) -> Result<Result<(), Map<String, Value>>, Gone> {
    let key = cx.agent.key();
    let app_name = app.name.clone();
    let new_session = cx.broker.run(|c| c.session(cx.agent).is_none());
    waiting(
        cx,
        &if new_session {
            format!("whether this agent may connect and use '{app_name}'")
        } else {
            format!("for permission to use '{app_name}'")
        },
    );
    let (agent, cwd, via, target) = (
        cx.agent.clone(),
        cx.cwd.map(str::to_owned),
        cx.via,
        app.conn_id.clone(),
    );
    let conn_id = app.conn_id.clone();
    // One question for all the instances of an application.
    let program = match &app.key {
        Some(app_key) => format!("app:{app_key}"),
        None => format!("conn:{conn_id}"),
    };
    let verdict = cx
        .broker
        .agent_consent(
            &format!("use:{key}:{program}"),
            |id, c| {
                let app = c.app_by_id(&conn_id).unwrap_or(app);
                let statuses = app.tools.iter().map(|t| c.status_of(app, t)).collect();
                texts::use_app(
                    id,
                    &texts::UseQuestion {
                        agent: cx.agent,
                        cwd: cx.cwd,
                        new_session,
                        app,
                        statuses,
                    },
                )
            },
            move |c, reply| {
                if !c.registered(&target) {
                    return;
                }
                match reply {
                    Reply::Block => c.block(&agent, &target),
                    Reply::Allow { .. } => {
                        c.allow_use(&agent, cwd.as_deref(), via, &target);
                    }
                    Reply::Deny => {}
                }
            },
            None,
            cx.gone,
        )
        .await?;
    let what = format!("use '{app_name}'");
    Ok(match verdict {
        Asked::Verdict(Verdict::Allowed) => Ok(()),
        Asked::Verdict(Verdict::Blocked) => Err(blocked(&app_name)),
        Asked::Verdict(verdict) => Err(refused(verdict, &what)),
        Asked::Link(link) => Err(pending(&link, &what)),
    })
}

/// A call the user is asked about.
struct CallAsk<'a> {
    /// The call: agent, program, command (as it is) and arguments.
    key: &'a str,
    app: &'a crate::core::AppConn,
    tool: &'a ToolDef,
    arguments: Option<&'a Map<String, Value>>,
    status: Status,
    restricted: bool,
}

/// One call of a command the user hasn't approved, asked of the user. Ok when it may run:
/// the user allowed it (this call, the command for the session, or approved it in Clak), or
/// another answer covered it meanwhile.
async fn ask_call(
    cx: &Context<'_>,
    call: &CallAsk<'_>,
) -> Result<Result<(), Map<String, Value>>, Gone> {
    let (app, tool) = (call.app, call.tool);
    let key = cx.agent.key();
    let app_name = app.name.clone();
    waiting(
        cx,
        &format!("for permission to run '{app_name} {}'", tool.name),
    );
    // One window at a time per agent and program: this call waits for the one open, then
    // is looked at again (the answer may cover it). On the web page nobody waits.
    if cx.broker.options().question_link.is_none()
        && let Some(mut other) = cx.broker.run(|c| c.open_call(&key, &app.conn_id, call.key))
    {
        tokio::select! {
            _ = other.wait_for(Option::is_some) => {}
            _ = cx.gone.cancelled() => return Err(Gone),
        }
        return Ok(Ok(()));
    }
    // The command as the question shows it: the one "Approve" approves.
    let shown: Arc<OnceLock<ToolDef>> = Arc::default();
    let shown_then = shown.clone();
    let cache_key = cx.broker.run(|c| c.cache_key(app).map(|(key, ..)| key));
    let (agent, target, call_for, cache_for) = (
        cx.agent.clone(),
        app.conn_id.clone(),
        call.key.to_owned(),
        cache_key.clone(),
    );
    let conn_id = app.conn_id.clone();
    let (status, arguments, restricted) = (call.status, call.arguments, call.restricted);
    let verdict = cx
        .broker
        .agent_consent(
            call.key,
            |id, c| {
                let app = c.app_by_id(&conn_id).unwrap_or(app);
                // The command as it is now: if it changed since, the answer is for this one,
                // and the call asks again.
                let tool = app
                    .tools
                    .iter()
                    .find(|t| t.name == tool.name)
                    .unwrap_or(tool);
                let _ = shown.set(tool.clone());
                let changes = cache_key
                    .as_deref()
                    .and_then(|k| c.commands.approved(k, &tool.name))
                    .filter(|_| status == Status::Changed)
                    .map(|before| crate::commands::diff(&before.definition, &tool.to_json()))
                    .unwrap_or_default();
                texts::call(
                    id,
                    &texts::CallQuestion {
                        agent: cx.agent,
                        cwd: cx.cwd,
                        app,
                        tool,
                        arguments,
                        status,
                        changes,
                        approvable: cache_key.is_some(),
                        restricted,
                    },
                )
            },
            move |c, reply| {
                let Some(app) = c.app_by_id(&target) else {
                    return;
                };
                let Reply::Allow { scope, .. } = reply else {
                    if reply == Reply::Block {
                        c.block(&agent, &target);
                    }
                    return;
                };
                // Blocked meanwhile, from another question: the block wins.
                if c.blocks(&agent.key(), app) {
                    return;
                }
                let program = app.name.clone();
                let Some(shown) = shown_then.get() else {
                    return;
                };
                let session = agent.key();
                match scope {
                    // One run of this call: for the first of those waiting for this answer
                    // or, on the web page, for the agent's next identical call.
                    Scope::Once => c.permit(&call_for),
                    Scope::Session => {
                        c.allow_for_session(
                            &session,
                            &target,
                            &[(shown.name.clone(), shown.digest.clone())],
                        );
                    }
                    Scope::Approve => {
                        if let Some(cache) = &cache_for {
                            c.commands.approve(cache, shown, "question");
                            c.log(format!("approved: “{}” of {program}", shown.name));
                        }
                        // Nothing to keep it in, or the preset leaves it out: for this
                        // session, or it would ask again. Otherwise the approval alone,
                        // which the panel can take back.
                        if restricted || cache_for.is_none() {
                            c.allow_for_session(
                                &session,
                                &target,
                                &[(shown.name.clone(), shown.digest.clone())],
                            );
                        }
                        c.settle_calls(None);
                    }
                }
            },
            Some(CallTarget {
                session_key: key,
                conn_id: app.conn_id.clone(),
                tool: tool.name.clone(),
                digest: tool.digest.clone(),
                cache_key: cache_key.clone(),
                restricted: call.restricted,
            }),
            cx.gone,
        )
        .await?;
    let what = format!("run '{app_name} {}'", tool.name);
    Ok(match verdict {
        Asked::Verdict(Verdict::Allowed) => Ok(()),
        Asked::Verdict(Verdict::Blocked) => Err(blocked(&app_name)),
        Asked::Verdict(verdict) => Err(refused(verdict, &what)),
        Asked::Link(link) => Err(pending(&link, &what)),
    })
}

/// Seconds as the hub writes them: at most three decimals, no trailing zeros.
fn format_seconds(seconds: f64) -> String {
    let text = format!("{seconds:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}
