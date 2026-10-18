//! Program names for the command line: `clak notion search`. Usually the hub derives them
//! from the program's name; for a connector or an API the user can pick one, with the same
//! rules.

use regex::Regex;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

pub const MAX_LENGTH: usize = 24;

/// The hub's own program: its settings (`clak settings …`).
pub const SETTINGS: &str = "settings";

/// The words `clak` uses for itself: a program cannot have these names. «settings» belongs
/// to the hub: no program, connector or API can take its place.
const RESERVED: [&str; 5] = ["list", "status", "help", "version", SETTINGS];

static NOT_SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new("[^a-z0-9]+").unwrap());
static SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new("^[a-z0-9]+(-[a-z0-9]+)*$").unwrap());

/// The automatic name: lowercase, without accents or spaces, 24 characters at most.
/// «Notion (work)» becomes «notion-work».
pub fn from(name: &str) -> String {
    let plain: String = name.nfd().filter(|c| !is_combining_mark(*c)).collect();
    let lowered = plain.to_lowercase();
    let mut slug = NOT_SLUG
        .replace_all(&lowered, "-")
        .trim_matches('-')
        .to_owned();
    if slug.len() > MAX_LENGTH {
        slug = slug[..MAX_LENGTH].trim_end_matches('-').to_owned();
    }
    if slug.is_empty() || RESERVED.contains(&slug.as_str()) {
        slug = format!("app-{slug}").trim_end_matches('-').to_owned();
    }
    slug
}

/// What is wrong with a name chosen by the user, or nothing if it is fine (empty means automatic).
pub fn problem(command: Option<&str>) -> Option<String> {
    let command = command.filter(|c| !c.is_empty())?;
    if command.chars().count() > MAX_LENGTH {
        return Some(format!(
            "The command is too long ({MAX_LENGTH} characters at most)."
        ));
    }
    if !SLUG.is_match(command) {
        return Some("The command can only have lowercase letters, digits and single hyphens (e.g. notion-work).".into());
    }
    if RESERVED.contains(&command) {
        return Some(format!("“{command}” is a word clak uses for itself."));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_names() {
        assert_eq!(from("Notion (work)"), "notion-work");
        assert_eq!(from("Crème Brûlée"), "creme-brulee");
        assert_eq!(from("Settings"), "app-settings");
        assert_eq!(from("!!!"), "app");
        assert_eq!(
            from("a very long program name indeed yes"),
            "a-very-long-program-name"
        );
    }

    #[test]
    fn chosen_names() {
        assert!(problem(None).is_none());
        assert!(problem(Some("notion-work")).is_none());
        assert!(problem(Some("Notion")).is_some());
        assert!(problem(Some("list")).is_some());
    }
}
