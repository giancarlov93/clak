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

use crate::consent::{AgentIdentity, Verdict};
use crate::core::{Broker, Gone, Progress, outcome};
use crate::wire::{Malformed, ToolDef};
use crate::{flags, texts};
use clak_wire::json::{self};
use clak_wire::names;
use serde_json::{Map, Value, json};
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
             `timeout` or `xargs` in Git Bash: run clak directly (for time limits use --wait).",
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
    cx.tell.waiting("whether this agent may connect");
    let (agent, cwd, via) = (cx.agent.clone(), cx.cwd.map(str::to_owned), cx.via);
    let verdict = cx
        .broker
        .consent(
            &format!("session:{key}"),
            |id, _| texts::session(id, cx.agent, cx.cwd),
            move |c| {
                c.authorize(&agent, cwd.as_deref(), via);
            },
            cx.gone,
        )
        .await?;
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
    Ok(ok(object(
        json!({ "resources": cx.broker.run(|c| c.listing(&key)) }),
    )))
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
            "No connected program is named '{}'{} `clak list` shows the connected ones.",
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

    // The agent and the program: if permission is missing, a single question for both.
    let current = || {
        cx.broker
            .run(|c| c.grant_of(&key, &conn_id).map(|g| g.id.clone()))
    };
    let mut waited = false;
    let mut grant_id = current();
    if grant_id.is_none() {
        let new_session = cx.broker.run(|c| c.session(cx.agent).is_none());
        cx.tell.waiting(&if new_session {
            format!("whether this agent may connect and use '{app_name}'")
        } else {
            format!("for permission to use '{app_name}'")
        });
        waited = true;
        let (agent, cwd, via, target) = (
            cx.agent.clone(),
            cx.cwd.map(str::to_owned),
            cx.via,
            conn_id.clone(),
        );
        let verdict = cx
            .broker
            .consent(
                &format!("use:{key}:{conn_id}"),
                |id, c| {
                    texts::use_app(
                        id,
                        cx.agent,
                        cx.cwd,
                        new_session,
                        c.app_by_id(&conn_id).unwrap_or(&app),
                    )
                },
                move |c| {
                    if c.registered(&target) {
                        let session = c.authorize(&agent, cwd.as_deref(), via);
                        c.grant(&session, &target);
                    }
                },
                cx.gone,
            )
            .await?;
        if verdict != Verdict::Allowed {
            return Ok(Ok(refused(verdict, &format!("use '{app_name}'"))));
        }
        grant_id = current();
        if grant_id.is_none() {
            return Ok(Ok(error(
                "gone",
                &format!("'{app_name}' is no longer connected."),
            )));
        }
    }
    let grant_id = grant_id.expect("checked above");

    // The command: a new one, or one changed after consent, needs its own.
    let found = cx.broker.run(|c| {
        let app = c.app_by_id(&conn_id)?.clone();
        let tool = app
            .tools
            .iter()
            .find(|t| Some(t.name.as_str()) == tool_name)?
            .clone();
        let grant = c.grant_by_id(&grant_id);
        let changed = grant.is_some_and(|g| g.approved.contains_key(&tool.name));
        let approves = grant.is_some_and(|g| g.approves(&tool));
        Some((app, tool, changed, approves))
    });
    let Some((app, tool, changed, approves)) = found else {
        let answer = cx.broker.run(|c| {
            c.app_by_id(&conn_id)
                .map(|a| no_command(&a.name, tool_name, &a.tools))
        });
        return Ok(Ok(answer.unwrap_or_else(|| {
            error("gone", &format!("'{app_name}' was closed."))
        })));
    };
    if !approves {
        cx.tell.waiting(&if changed {
            format!(
                "for permission to use the command '{}', which changed since the last permission",
                tool.name
            )
        } else {
            format!("for permission to use the command '{}'", tool.name)
        });
        waited = true;
        let (grant_for, name_for, digest_for) =
            (grant_id.clone(), tool.name.clone(), tool.digest.clone());
        let verdict = cx
            .broker
            .consent(
                &format!("tool:{grant_id}:{}:{}", tool.name, tool.digest),
                |id, _| texts::tool(id, cx.agent, cx.cwd, &app, &tool, changed),
                move |c| {
                    if let Some(grant) = c.grant_by_id_mut(&grant_for) {
                        grant.approved.insert(name_for, digest_for);
                    }
                },
                cx.gone,
            )
            .await?;
        if verdict != Verdict::Allowed {
            return Ok(Ok(refused(
                verdict,
                &format!("use the command '{}'", tool.name),
            )));
        }
    }

    let begun = cx.broker.run(|c| {
        if !c.registered(&conn_id)
            || c.grant_of(&key, &conn_id).map(|g| g.id.as_str()) != Some(grant_id.as_str())
        {
            return None;
        }
        // Between consent and now the program may have changed the command once more.
        let current = c
            .app_by_id(&conn_id)?
            .tools
            .iter()
            .find(|t| t.name == tool.name)?
            .clone();
        if !c.grant_by_id(&grant_id)?.approves(&current) {
            return None;
        }
        c.begin_call(&grant_id, &current, arguments.take(), cx.tell.clone())
    });
    let Some((routed, answer)) = begun else {
        return Ok(Ok(error(
            "gone",
            "The permission or the command changed in the meantime: try again.",
        )));
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

/// Seconds as the hub writes them: at most three decimals, no trailing zeros.
fn format_seconds(seconds: f64) -> String {
    let text = format!("{seconds:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}
