//! The wire's JSON. No object serialization: only JSON trees travel on the wire, so what
//! a program declares (a schema, some arguments) crosses the hub intact.

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Beyond this size a message is not a message.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Reads JSON, rejecting two equal keys in the same object: between "first wins" and
/// "last wins" there is room to get one thing approved and pass another.
pub fn parse(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    let value = Strict.deserialize(&mut reader)?;
    reader.end()?;
    Ok(value)
}

/// The same, from a text.
pub fn parse_str(text: &str) -> Result<Value, serde_json::Error> {
    parse(text.as_bytes())
}

/// A JSON object from a line, or nothing if the line is not one.
pub fn object(text: &[u8]) -> Option<Map<String, Value>> {
    match parse(text) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        Strict.deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Strict)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key '{key}'")));
            }
            let value = map.next_value_seed(Strict)?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

/// Compact, without escaping non-ASCII characters.
pub fn encode(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec())
}

/// Compact, with the newline: one line of the control channel.
pub fn encode_line(value: &Value) -> Vec<u8> {
    let mut bytes = encode(value);
    bytes.push(b'\n');
    bytes
}

/// Indented, for files a person reads too.
pub fn encode_pretty(value: &Value) -> Vec<u8> {
    serde_json::to_vec_pretty(value).unwrap_or_else(|_| b"null".to_vec())
}

pub fn text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

/// Seconds since the epoch, with decimals.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// A floating-point number as the hub writes it: `60.0`, not `60`. Whoever reads the wire
/// from a language that tells integers apart sees the same type whatever the value.
pub fn float(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// A number rounded to three decimals, like the durations in the history.
pub fn round3(value: f64) -> Value {
    float((value * 1000.0).round() / 1000.0)
}

/// `prefix_` and 12 random hex digits.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", random_hex(6))
}

/// `bytes` random bytes in hex.
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("no system randomness");
    hex::encode(buffer)
}

/// The text of a value, if it is a string.
pub fn str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

/// The truth of a value as the hub sees it: empty, zero and null are false.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_none_or(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The fingerprint of a JSON value: sorted keys, compact form, sha256, 16 digits.
/// It only needs to be stable within this process: nobody else recomputes it.
pub fn digest(value: &Value) -> String {
    let mut bytes = Vec::new();
    write_sorted(&mut bytes, value);
    hex::encode(Sha256::digest(&bytes))[..16].to_owned()
}

fn write_sorted(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Object(map) => {
            out.push(b'{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend(serde_json::to_vec(key).unwrap_or_default());
                out.push(b':');
                write_sorted(out, &map[key.as_str()]);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_sorted(out, item);
            }
            out.push(b']');
        }
        other => out.extend(encode(other)),
    }
}

/// A list of names as the log writes it: `['a', 'b']`.
pub fn quoted<S: AsRef<str>>(names: &[S]) -> String {
    let inner: Vec<String> = names.iter().map(|n| format!("'{}'", n.as_ref())).collect();
    format!("[{}]", inner.join(", "))
}

/// Helpers for reading an object's fields.
pub trait ObjectExt {
    fn str_of(&self, key: &str) -> Option<&str>;
    fn bool_of(&self, key: &str) -> bool;
}

impl ObjectExt for Map<String, Value> {
    fn str_of(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    fn bool_of(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
}

impl ObjectExt for Value {
    fn str_of(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    fn bool_of(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn due_chiavi_uguali_non_si_leggono() {
        assert!(parse(br#"{"a":1,"a":2}"#).is_err());
        assert!(parse(br#"{"a":{"b":1,"b":1}}"#).is_err());
        assert_eq!(
            parse(br#"{"a":1,"b":[1,2]}"#).unwrap(),
            json!({"a":1,"b":[1,2]})
        );
    }

    #[test]
    fn i_numeri_in_virgola_restano_in_virgola() {
        assert_eq!(text(&float(60.0)), "60.0");
        assert_eq!(text(&float(1.5)), "1.5");
    }

    #[test]
    fn l_impronta_non_dipende_dall_ordine_delle_chiavi() {
        assert_eq!(digest(&json!({"a":1,"b":2})), digest(&json!({"b":2,"a":1})));
        assert_ne!(digest(&json!({"a":1})), digest(&json!({"a":2})));
        assert_eq!(digest(&json!({})).len(), 16);
    }

    #[test]
    fn la_verita_dei_valori() {
        assert!(!truthy(Some(&json!(""))));
        assert!(!truthy(Some(&json!(0))));
        assert!(!truthy(Some(&json!({}))));
        assert!(truthy(Some(&json!("x"))));
        assert!(truthy(Some(&json!([1]))));
    }

    #[test]
    fn i_testi_non_ascii_non_si_scappano() {
        assert_eq!(text(&json!("perché")), "\"perché\"");
    }
}
