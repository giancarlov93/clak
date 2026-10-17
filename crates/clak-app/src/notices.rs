//! What is worth telling the user even with the panel closed, with a system
//! notification: a program connecting, an agent starting or stopping using it. And the
//! short sentences with which the tray and the menu describe the state.

use serde_json::Value;

/// A notification: title and text.
pub struct Notice {
    pub title: String,
    pub text: String,
}

fn list<'a>(state: &'a Value, key: &str) -> &'a [Value] {
    state.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
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
pub fn between(before: Option<&Value>, after: &Value) -> Vec<Notice> {
    let mut notices = Vec::new();
    let Some(before) = before else { return notices };

    for resource in list(after, "resources").iter().filter(|r| !has(before, "resources", text(r, "id"))) {
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
        find(state, "sessions", text(grant, "session_id")).map_or("An agent".to_owned(), |s| text(s, "label").to_owned())
    };
    for grant in list(after, "grants").iter().filter(|g| !has(before, "grants", text(g, "id"))) {
        let Some(resource) = find(after, "resources", text(grant, "resource_id")) else { continue };
        notices.push(Notice {
            title: format!("“{}” in use", text(&resource["resource"], "title")),
            text: format!("{} can use {}.", agent(after, grant), text(&resource["resource"], "app")),
        });
    }
    for grant in list(before, "grants").iter().filter(|g| !has(after, "grants", text(g, "id"))) {
        let Some(resource) = find(before, "resources", text(grant, "resource_id")) else { continue };
        notices.push(Notice {
            title: format!("“{}” no longer in use", text(&resource["resource"], "title")),
            text: format!("{} can no longer use {}.", agent(before, grant), text(&resource["resource"], "app")),
        });
    }
    notices
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

/// The "Revoke …" menu items: permission id and text.
pub fn revocations(state: &Value) -> Vec<(String, String)> {
    list(state, "grants")
        .iter()
        .map(|grant| {
            let title = find(state, "resources", text(grant, "resource_id"))
                .map_or_else(|| text(grant, "resource_id").to_owned(), |r| text(&r["resource"], "title").to_owned());
            let from = find(state, "sessions", text(grant, "session_id"))
                .map(|s| format!(" from {}", text(s, "label")))
                .unwrap_or_default();
            (text(grant, "id").to_owned(), format!("Revoke “{title}”{from}"))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn le_notifiche_raccontano_i_cambiamenti() {
        let empty = json!({"resources": [], "grants": [], "sessions": []});
        let connected = json!({
            "resources": [{"id": "r1", "name": "blocco", "resource": {"app": "Blocco", "title": "spesa.txt"}}],
            "sessions": [{"id": "s1", "label": "Claude Code · prova"}],
            "grants": [{"id": "g1", "resource_id": "r1", "session_id": "s1"}],
        });
        assert!(between(None, &connected).is_empty());
        let notices = between(Some(&empty), &connected);
        assert_eq!(notices[0].title, "Blocco connected");
        assert_eq!(notices[1].title, "“spesa.txt” in use");
        assert_eq!(notices[1].text, "Claude Code · prova can use Blocco.");
        let gone = between(Some(&connected), &json!({"resources": connected["resources"], "sessions": [], "grants": []}));
        assert_eq!(gone[0].title, "“spesa.txt” no longer in use");
        assert_eq!(headline(&connected), "1 agent · 1 program · 1 permission");
    }
}
