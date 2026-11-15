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

use crate::consent::{AgentIdentity, Reply, Scope, Verdict};
use crate::core::{Asked, Broker, CallTarget, Core, Gone, Progress, outcome};
use crate::wire::{Malformed, ToolDef};
use crate::{flags, rules, texts};
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
    let agent_key = agent_key(cx).await;
    Ok(ok(object(json!({
        "resources": cx.broker.run(|c| c.listing(&key, agent_key.as_deref(), cx.cwd)),
    }))))
}

/// Who the agent is, for the standing rules: read once per process, outside the lock (an
/// unsigned agent's content is hashed). None if it can't be told.
async fn agent_key(cx: &Context<'_>) -> Option<String> {
    let process = cx.agent.key();
    if let Some(known) = cx.broker.run(|c| c.agent_keys.get(&process).cloned()) {
        return known;
    }
    let platform = cx.broker.options().platform.clone();
    let agent = cx.agent.clone();
    let found = tokio::task::spawn_blocking(move || rules::agent_key(platform.as_ref(), &agent))
        .await
        .ok()
        .flatten();
    cx.broker
        .run(|c| c.agent_keys.insert(process, found.clone()));
    found
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

fn no_command(app_name: &str, name: Option<&str>, tools: &[ToolDef]) -> Map<String, Value> {
    let near = names::closest(name, tools.iter().map(|t| t.name.as_str()));
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
        return Ok(Ok(no_command(&app_name, tool_name, &app.tools)));
    };
    let mut arguments = request.get("arguments").and_then(Value::as_object).cloned();
    if let Some(Value::Array(flags)) = request.get("flags") {
        match flags::apply(asked, arguments.as_ref(), flags) {
            Ok(applied) => arguments = Some(applied),
            Err(problem) => return Ok(Ok(error("bad_request", &problem))),
        }
    }

    // A program the user blocked for this agent: refused, without a question.
    if cx.broker.run(|c| c.blocks(&key, &app)) {
        return Ok(Ok(blocked(&app_name)));
    }
    let current = || {
        cx.broker
            .run(|c| c.grant_of(&key, &conn_id).map(|g| g.id.clone()))
    };
    let mut waited = false;
    let mut grant_id = current();
    if grant_id.is_none() {
        // Launched with a preset that covers the program: its commands need no question.
        grant_id = cx.broker.run(|c| c.preset_grant(&key, &conn_id));
    }
    // Who the agent is, for the standing rules: read outside the lock (an unsigned agent's
    // content is hashed), once per process.
    let agent_key = agent_key(cx).await;
    if grant_id.is_none()
        && let Some(agent_key) = &agent_key
    {
        // A rule the user made standing: its commands need no question either.
        grant_id = cx
            .broker
            .run(|c| c.rule_grant(agent_key, cx.agent, cx.cwd, cx.via, &conn_id));
    }
    if grant_id.is_none() && app.is_hub() {
        // Clak's settings: one question for the session, since every change asks anyway.
        match use_settings(cx, &app).await? {
            Ok(()) => waited = true,
            Err(answer) => return Ok(Ok(answer)),
        }
    }

    // Until the command runs or the user refuses: the command as the program has it now;
    // whether the session allows it (or a preset, or a standing rule, does); otherwise this
    // call asks the user. The same call (agent, program, command and arguments) asked twice
    // waits for the same answer; "only this call" lets one of them run, the others ask again.
    let arguments_digest = json::digest(&Value::Object(arguments.clone().unwrap_or_default()));
    let (routed, answer, tool) = loop {
        let found = cx.broker.run(|c| {
            let app = c.app_by_id(&conn_id)?.clone();
            let tool = app
                .tools
                .iter()
                .find(|t| Some(t.name.as_str()) == tool_name)?
                .clone();
            Some((app, tool))
        });
        let Some((app, tool)) = found else {
            let answer = cx.broker.run(|c| {
                c.app_by_id(&conn_id)
                    .map(|a| no_command(&a.name, tool_name, &a.tools))
            });
            return Ok(Ok(answer.unwrap_or_else(|| {
                error("gone", &format!("'{app_name}' was closed."))
            })));
        };
        let call_key = format!(
            "call:{key}:{conn_id}:{}:{}:{arguments_digest}",
            tool.name, tool.digest
        );
        let begun = cx.broker.run(|c| {
            let app = c.app_by_id(&conn_id)?;
            if c.blocks(&key, app) {
                return Some(Err(()));
            }
            let allowed = c
                .grant_of(&key, &conn_id)
                .is_some_and(|g| g.approves(&tool))
                || c.standing(&key, agent_key.as_deref(), cx.cwd, &conn_id, &tool)
                || (c.grant_of(&key, &conn_id).is_some() && c.take_permit(&call_key));
            if !allowed {
                return None;
            }
            let grant_id = c.grant_of(&key, &conn_id)?.id.clone();
            c.begin_call(&grant_id, &tool, arguments.take(), cx.tell.clone())
                .map(Ok)
        });
        match begun {
            Some(Ok((routed, answer))) => break (routed, answer, tool),
            Some(Err(())) => return Ok(Ok(blocked(&app_name))),
            None => {}
        }
        let note = cx.broker.run(|c| match c.grant_of(&key, &conn_id) {
            Some(g) if g.approved.contains_key(&tool.name) => Some("changed"),
            Some(g) if g.all => Some("new"),
            _ => None,
        });
        match ask_call(
            cx,
            &call_key,
            &app,
            &tool,
            arguments.as_ref(),
            note,
            agent_key.clone(),
        )
        .await?
        {
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
/// propose changes. Ok when the agent may go on.
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
                    let session = c.authorize(&agent, cwd.as_deref(), via);
                    let shown = shown_then.get().map_or(&[][..], Vec::as_slice);
                    c.allow_call(&session, &target, shown, true);
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

/// One call, asked of the user. Ok when it may run: the user allowed it (for this call, the
/// command, or all the program's commands), or another answer covered it meanwhile.
async fn ask_call(
    cx: &Context<'_>,
    call_key: &str,
    app: &crate::core::AppConn,
    tool: &ToolDef,
    arguments: Option<&Map<String, Value>>,
    note: Option<&'static str>,
    agent_key: Option<String>,
) -> Result<Result<(), Map<String, Value>>, Gone> {
    let key = cx.agent.key();
    let app_name = app.name.clone();
    let new_session = cx.broker.run(|c| c.session(cx.agent).is_none());
    waiting(
        cx,
        &if new_session {
            format!(
                "whether this agent may connect and run '{app_name} {}'",
                tool.name
            )
        } else {
            format!("for permission to run '{app_name} {}'", tool.name)
        },
    );
    // "Always": offered only for an agent and a program that can be recognized next time:
    // an application by its key, a connector or an API by the name the user gave it.
    let agent_key = agent_key.filter(|_| app.key.is_some() || app.is_connector());
    // "All the commands" allows those the question shows, as they are when it is asked
    // (see `use_settings`).
    let shown: Arc<OnceLock<Vec<(String, String)>>> = Arc::default();
    let shown_then = shown.clone();
    let (agent, cwd, via, target) = (
        cx.agent.clone(),
        cx.cwd.map(str::to_owned),
        cx.via,
        app.conn_id.clone(),
    );
    let (name, digest, call_for) = (tool.name.clone(), tool.digest.clone(), call_key.to_owned());
    let standing = agent_key.clone();
    let offer = standing.is_some();
    let conn_id = app.conn_id.clone();
    let verdict = cx
        .broker
        .agent_consent(
            call_key,
            |id, c| {
                let app = c.app_by_id(&conn_id).unwrap_or(app);
                let _ = shown.set(
                    app.tools
                        .iter()
                        .map(|t| (t.name.clone(), t.digest.clone()))
                        .collect(),
                );
                // The command as it is now: the one "all the commands" allows. If it changed
                // since, "only this call" and "this command" don't match it, and it asks again.
                let tool = app
                    .tools
                    .iter()
                    .find(|t| t.name == tool.name)
                    .unwrap_or(tool);
                texts::call(
                    id,
                    &texts::CallQuestion {
                        agent: cx.agent,
                        cwd: cx.cwd,
                        new_session,
                        app,
                        tool,
                        arguments,
                        note,
                        standing: offer,
                    },
                )
            },
            move |c, reply| {
                let Some(app) = c.app_by_id(&target) else {
                    return;
                };
                let Reply::Allow { scope, always } = reply else {
                    if reply == Reply::Block {
                        c.block(&agent, &target);
                    }
                    return;
                };
                // Blocked meanwhile, from another question: the block wins.
                if c.blocks(&agent.key(), app) {
                    return;
                }
                let session = c.authorize(&agent, cwd.as_deref(), via);
                let shown = shown_then.get().map_or(&[][..], Vec::as_slice);
                match scope {
                    Scope::Once => {
                        // One run of this call: for the first of those waiting for this
                        // answer or, on the web page, for the agent's next identical call.
                        c.allow_call(&session, &target, &[], false);
                        c.permit(&call_for);
                    }
                    Scope::Command => {
                        c.allow_call(&session, &target, &[(name.clone(), digest)], false);
                    }
                    Scope::Program => {
                        c.allow_call(&session, &target, shown, true);
                    }
                }
                if always
                    && scope != Scope::Once
                    && let Some(agent_key) = &agent_key
                {
                    let command = (scope == Scope::Command).then_some(name);
                    remember(c, agent_key, &agent, cwd.as_deref(), &target, command);
                }
            },
            Some(CallTarget {
                session_key: key,
                conn_id: app.conn_id.clone(),
                tool: tool.name.clone(),
                digest: tool.digest.clone(),
                agent_key: standing,
                cwd: cx.cwd.map(str::to_owned),
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

/// "Always" ticked with a yes: the standing rule, for the command or for all of them.
fn remember(
    c: &mut Core,
    agent_key: &str,
    agent: &AgentIdentity,
    cwd: Option<&str>,
    conn_id: &str,
    command: Option<String>,
) {
    let Some(app) = c.app_by_id(conn_id).cloned() else {
        return;
    };
    let line = {
        let rule = c.rules.add(agent_key, agent, cwd, &app, command);
        let what = match &rule.program.tools {
            Some(tools) => format!("«{}» ({})", rule.label, tools.join(", ")),
            None => format!("«{}»", rule.label),
        };
        match &rule.folder {
            Some(folder) => format!(
                "always allowed from now: {} on {what} in {folder}",
                rule.agent
            ),
            None => format!("always allowed from now: {} on {what}", rule.agent),
        }
    };
    c.log(line);
    c.touch();
}

/// Seconds as the hub writes them: at most three decimals, no trailing zeros.
fn format_seconds(seconds: f64) -> String {
    let text = format!("{seconds:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}
