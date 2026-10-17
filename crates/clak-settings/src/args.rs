//! The arguments of a command, read with a clear error if the type is wrong. A `null`
//! argument is the same as a missing one.

use crate::{Stop, say};
use serde_json::{Map, Value};

pub(crate) struct Args<'a>(&'a Map<String, Value>);

impl<'a> Args<'a> {
    pub fn new(json: &'a Map<String, Value>) -> Args<'a> {
        Args(json)
    }

    pub fn has(&self, name: &str) -> bool {
        self.node(name).is_some()
    }

    pub fn node(&self, name: &str) -> Option<&'a Value> {
        self.0.get(name).filter(|v| !v.is_null())
    }

    pub fn text(&self, name: &str) -> Result<Option<String>, Stop> {
        match self.node(name) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(say(format!("--{} must be text.", flag(name)))),
        }
    }

    pub fn required(&self, name: &str) -> Result<String, Stop> {
        match self.text(name)? {
            Some(text) if !text.trim().is_empty() => Ok(text.trim().to_owned()),
            _ => Err(say(format!("--{} is required.", flag(name)))),
        }
    }

    pub fn bool(&self, name: &str) -> Result<Option<bool>, Stop> {
        match self.node(name) {
            None => Ok(None),
            Some(Value::Bool(on)) => Ok(Some(*on)),
            Some(_) => Err(say(format!("--{} is true or false.", flag(name)))),
        }
    }

    /// A list; a single value becomes a list of one.
    pub fn list(&self, name: &str) -> Option<Vec<Value>> {
        match self.node(name)? {
            Value::Array(list) => Some(list.clone()),
            one => Some(vec![one.clone()]),
        }
    }
}

/// The name as the command line writes it: `base_url` → `base-url`.
fn flag(name: &str) -> String {
    name.replace('_', "-")
}
