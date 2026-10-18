//! What is worth telling the user even with the panel closed, with a system
//! notification: a program connecting, an agent starting or stopping using it. And the
//! short sentences with which the tray and the menu describe the state.

use serde_json::Value;

/// A notification: title and text.
pub struct Notice {
    pub title: String,
    pub text: String,
}

/// Connection feedback is independent of whether system notifications are enabled.
#[derive(Default)]
pub struct Changes {
    pub notices: Vec<Notice>,
    pub agent_connected: bool,
}

fn list<'a>(state: &'a Value, key: &str) -> &'a [Value] {
    state
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn find<'a>(state: &'a Value, key: &str, id: &str) -> Option<&'a Value> {
    list(state, key).iter().find(|v| text(v, "id") == id)
}

fn has(state: &Value, key: &str, id: &str) -> bool {
    find(state, key, id).is_some()
}

/// What happened between two snapshots. The first one reports nothing: it is just the start.
pub fn between(before: Option<&Value>, after: &Value) -> Changes {
    let Some(before) = before else {
        return Changes::default();
    };
    let mut changes = Changes {
        agent_connected: after.get("offline").is_none()
            && list(after, "sessions")
                .iter()
                .any(|s| !has(before, "sessions", text(s, "id"))),
        ..Changes::default()
    };
    let notices = &mut changes.notices;

    for resource in list(after, "resources")
        .iter()
        .filter(|r| !has(before, "resources", text(r, "id")))
    {
        let info = &resource["resource"];
        notices.push(Notice {
            title: format!("{} connected", text(info, "app")),
            text: format!(
                "“{}” is available to agents as “{}”. Each one will have to ask your permission to use it.",
                text(info, "title"),
                text(resource, "name")
            ),
        });
    }
    let agent = |state: &Value, grant: &Value| {
        find(state, "sessions", text(grant, "session_id"))
            .map_or("An agent".to_owned(), |s| text(s, "label").to_owned())
    };
    for grant in list(after, "grants")
        .iter()
        .filter(|g| !has(before, "grants", text(g, "id")))
    {
        let Some(resource) = find(after, "resources", text(grant, "resource_id")) else {
            continue;
        };
        notices.push(Notice {
            title: format!("“{}” in use", text(&resource["resource"], "title")),
            text: format!(
                "{} can use {}.",
                agent(after, grant),
                text(&resource["resource"], "app")
            ),
        });
    }
    for grant in list(before, "grants")
        .iter()
        .filter(|g| !has(after, "grants", text(g, "id")))
    {
        let Some(resource) = find(before, "resources", text(grant, "resource_id")) else {
            continue;
        };
        notices.push(Notice {
            title: format!(
                "“{}” no longer in use",
                text(&resource["resource"], "title")
            ),
            text: format!(
                "{} can no longer use {}.",
                agent(before, grant),
                text(&resource["resource"], "app")
            ),
        });
    }
    changes
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "1 agent · 1 program · 1 permission".
pub fn headline(state: &Value) -> String {
    [
        plural(list(state, "sessions").len(), "agent", "agents"),
        plural(list(state, "resources").len(), "program", "programs"),
        plural(list(state, "grants").len(), "permission", "permissions"),
    ]
    .join(" · ")
}

/// A connected agent and the programs it may use, as the menu shows them.
#[derive(Clone, PartialEq)]
pub struct Agent {
    pub label: String,
    pub exe: Option<String>,
    pub programs: Vec<Program>,
}

#[derive(Clone, PartialEq)]
pub struct Program {
    pub grant: String,
    /// "groceries.txt (Notepad)", or just "Blender" when the title is the program's name.
    pub label: String,
    pub exe: Option<String>,
    /// An MCP server or a REST API: `mcp:<id>` or `api:<id>`, for its icon (server_icon.rs).
    pub icon: Option<String>,
}

fn since(value: &Value) -> f64 {
    value.get("since").and_then(Value::as_f64).unwrap_or(0.0)
}

/// Who is using what: the agents in the order they connected, each with its programs.
pub fn agents(state: &Value) -> Vec<Agent> {
    let mut sessions: Vec<&Value> = list(state, "sessions").iter().collect();
    sessions.sort_by(|a, b| since(a).total_cmp(&since(b)));
    sessions
        .into_iter()
        .map(|session| {
            let mut grants: Vec<&Value> = list(state, "grants")
                .iter()
                .filter(|g| text(g, "session_id") == text(session, "id"))
                .collect();
            grants.sort_by(|a, b| since(a).total_cmp(&since(b)));
            let programs = grants
                .into_iter()
                .map(|grant| {
                    let resource = find(state, "resources", text(grant, "resource_id"));
                    let info = resource.map(|r| &r["resource"]);
                    let (title, app) = info.map_or((text(grant, "resource_id"), ""), |i| {
                        (text(i, "title"), text(i, "app"))
                    });
                    let label = if app.is_empty() || app == title {
                        title.to_owned()
                    } else {
                        format!("{title} ({app})")
                    };
                    // Connectors and the hub's own programs have no executable of their own.
                    let exe = resource
                        .filter(|r| r["origin"].get("kind").is_none())
                        .and_then(|r| r["origin"].get("exe"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    Program {
                        grant: text(grant, "id").to_owned(),
                        label,
                        exe,
                        icon: if resource.is_some_and(|r| text(&r["origin"], "kind") == "hub") {
                            Some(crate::server_icon::CLAK.to_owned())
                        } else {
                            icon_source(state, text(grant, "resource_id"))
                        },
                    }
                })
                .collect();
            Agent {
                label: text(session, "label").to_owned(),
                exe: session
                    .get("exe")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                programs,
            }
        })
        .collect()
}

/// The MCP server or REST API behind a resource, as `mcp:<id>` or `api:<id>`.
pub fn icon_source(state: &Value, resource_id: &str) -> Option<String> {
    [("connectors", "mcp"), ("apis", "api")]
        .into_iter()
        .find_map(|(key, kind)| {
            list(state, key)
                .iter()
                .find(|e| !resource_id.is_empty() && text(e, "resource_id") == resource_id)
                .map(|e| format!("{kind}:{}", text(e, "id")))
        })
}

pub fn pending(state: &Value) -> usize {
    list(state, "pending").len()
}

pub fn grants(state: &Value) -> usize {
    list(state, "grants").len()
}

pub fn busy(state: &Value) -> usize {
    list(state, "sessions").len() + list(state, "resources").len()
}

/// The agents, and the programs connected from outside: what a restart cuts off. The hub's
/// own programs and the connectors come back by themselves.
pub fn connected(state: &Value) -> usize {
    let programs = list(state, "resources")
        .iter()
        .filter(|r| r["origin"].get("kind").is_none())
        .count();
    list(state, "sessions").len() + programs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_hub_s_own_programs_and_connectors_are_not_connections() {
        let idle = json!({"sessions": [], "resources": [
            {"id": "r1", "origin": {"kind": "hub"}},
            {"id": "r2", "origin": {"kind": "connector", "protocol": "mcp"}},
        ]});
        assert_eq!(connected(&idle), 0);
        let busy = json!({"sessions": [{"id": "s1"}], "resources": [
            {"id": "r1", "origin": {"kind": "hub"}},
            {"id": "r3", "origin": {"exe": r"C:\Program Files\Blender\blender.exe"}},
        ]});
        assert_eq!(connected(&busy), 2);
    }

    #[test]
    fn only_new_agent_sessions_trigger_connection_feedback() {
        let empty = json!({"sessions": [], "resources": [], "grants": []});
        let program = json!({"sessions": [], "resources": [{"id": "r1"}]});
        let connected = json!({"sessions": [{"id": "s1"}, {"id": "s2"}]});
        assert!(!between(None, &connected).agent_connected);
        assert!(!between(Some(&empty), &program).agent_connected);
        assert!(between(Some(&empty), &connected).agent_connected);
        assert!(!between(Some(&connected), &connected).agent_connected);
        assert!(!between(Some(&connected), &empty).agent_connected);
        let replacement = json!({"sessions": [{"id": "s3"}, {"id": "s2"}]});
        assert!(between(Some(&connected), &replacement).agent_connected);
        let grant = json!({"sessions": connected["sessions"], "grants": [{"id": "g1"}]});
        assert!(!between(Some(&connected), &grant).agent_connected);
    }

    #[test]
    fn notices_describe_the_changes() {
        let empty = json!({"resources": [], "grants": [], "sessions": []});
        let connected = json!({
            "resources": [{"id": "r1", "name": "notepad", "resource": {"app": "Notepad", "title": "groceries.txt"}}],
            "sessions": [{"id": "s1", "label": "Claude Code · demo"}],
            "grants": [{"id": "g1", "resource_id": "r1", "session_id": "s1"}],
        });
        assert!(between(None, &connected).notices.is_empty());
        let notices = between(Some(&empty), &connected).notices;
        assert_eq!(notices[0].title, "Notepad connected");
        assert_eq!(notices[1].title, "“groceries.txt” in use");
        assert_eq!(notices[1].text, "Claude Code · demo can use Notepad.");
        let gone = between(
            Some(&connected),
            &json!({"resources": connected["resources"], "sessions": [], "grants": []}),
        );
        assert_eq!(gone.notices[0].title, "“groceries.txt” no longer in use");
        assert_eq!(headline(&connected), "1 agent · 1 program · 1 permission");
    }

    #[test]
    fn each_agent_carries_its_own_programs() {
        let state = json!({
            "resources": [
                {"id": "r1", "resource": {"app": "Notepad", "title": "groceries.txt"}, "origin": {"exe": "/A/Notepad"}},
                {"id": "r2", "resource": {"app": "Rhino", "title": "Rhino"}, "origin": {"kind": "connector", "exe": "/x", "url": "https://mcp.rhino.test/mcp"}},
                {"id": "r3", "resource": {"app": "Shop", "title": "Shop"}, "origin": {"kind": "connector", "protocol": "rest", "url": "https://api.shop.test"}},
                {"id": "r4", "resource": {"app": "Clak", "title": "Clak — Settings"}, "origin": {"kind": "hub"}},
            ],
            "connectors": [{"id": "cn_rhino", "resource_id": "r2"}, {"id": "cn_off", "resource_id": null}],
            "apis": [{"id": "api_shop", "resource_id": "r3"}],
            "sessions": [
                {"id": "s2", "label": "Codex", "since": 2.0},
                {"id": "s1", "label": "Claude Code", "exe": "/C/claude", "since": 1.0},
            ],
            "grants": [
                {"id": "g2", "resource_id": "r2", "session_id": "s1", "since": 4.0},
                {"id": "g1", "resource_id": "r1", "session_id": "s1", "since": 3.0},
                {"id": "g3", "resource_id": "r3", "session_id": "s1", "since": 5.0},
                {"id": "g4", "resource_id": "r4", "session_id": "s1", "since": 6.0},
            ],
        });
        let agents = agents(&state);
        assert_eq!(
            agents.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(),
            ["Claude Code", "Codex"]
        );
        assert_eq!(agents[0].exe.as_deref(), Some("/C/claude"));
        let programs: Vec<_> = agents[0]
            .programs
            .iter()
            .map(|p| {
                (
                    p.grant.as_str(),
                    p.label.as_str(),
                    p.exe.as_deref(),
                    p.icon.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            programs,
            [
                ("g1", "groceries.txt (Notepad)", Some("/A/Notepad"), None),
                ("g2", "Rhino", None, Some("mcp:cn_rhino")),
                ("g3", "Shop", None, Some("api:api_shop")),
                ("g4", "Clak — Settings (Clak)", None, Some("clak"))
            ]
        );
        assert!(agents[1].programs.is_empty());
    }
}
