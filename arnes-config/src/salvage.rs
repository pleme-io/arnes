//! Scoped refusal for a whole config document.
//!
//! serde refuses a document when one value in it does not fit, and shikumi
//! then has nothing to hand back: one mistyped number used to cost every
//! other setting in the file. [`salvage`] applies a document to a base value
//! one key at a time and keeps each key only if the result still
//! deserializes. A key that does not fit is refused by its path, with serde's
//! reason, and its default is kept; an object is descended into, so the bad
//! leaf is refused rather than its section; a list keeps every entry that
//! fits by itself. Unknown keys (which serde would drop without a word) are
//! named too, with the nearest known key.
//!
//! Nothing here knows [`crate::ArnesConfig`]: any `Serialize +
//! DeserializeOwned` type with a JSON schema works.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// What a salvaged load refused, each by its dotted path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refusals(pub Vec<String>);

impl Refusals {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn fits<T: DeserializeOwned>(v: &Value) -> Result<(), String> {
    serde_json::from_value::<T>(v.clone()).map(|_| ()).map_err(|e| e.to_string())
}

fn slot<'a>(root: &'a mut Value, path: &[String]) -> &'a mut Value {
    let mut cur = root;
    for k in path {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        cur = cur.as_object_mut().expect("made an object").entry(k.clone()).or_insert(Value::Null);
    }
    cur
}

fn dotted(path: &[String]) -> String {
    path.join(".")
}

/// Apply `overlay` onto `base` key by key; see the module docs.
///
/// # Panics
/// Never for a `T` whose own serialization deserializes (every config type).
pub fn salvage<T: Serialize + DeserializeOwned>(base: &T, overlay: &Value) -> (T, Refusals) {
    let mut root = serde_json::to_value(base).unwrap_or(Value::Object(Map::new()));
    let mut refused = Vec::new();
    if let Value::Object(map) = overlay {
        apply::<T>(&mut root, &[], map, &mut refused);
    } else if !overlay.is_null() {
        refused.push(format!("the document is not a set of keys (got {overlay}); using defaults"));
    }
    let value = serde_json::from_value::<T>(root).unwrap_or_else(|_| {
        serde_json::from_value(serde_json::to_value(base).expect("base serializes")).expect("base round-trips")
    });
    (value, Refusals(refused))
}

fn apply<T: DeserializeOwned>(root: &mut Value, path: &[String], map: &Map<String, Value>, refused: &mut Vec<String>) {
    for (k, v) in map {
        let mut here = path.to_vec();
        here.push(k.clone());
        let mut candidate = root.clone();
        *slot(&mut candidate, &here) = v.clone();
        let whole = fits::<T>(&candidate);
        match (whole, v) {
            (Ok(()), _) => *root = candidate,
            (Err(_), Value::Object(inner)) => {
                // Descend: refuse the leaf, keep the section.
                let mut prepared = root.clone();
                let at = slot(&mut prepared, &here);
                if !at.is_object() {
                    *at = Value::Object(Map::new());
                }
                if fits::<T>(&prepared).is_ok() {
                    *root = prepared;
                    apply::<T>(root, &here, inner, refused);
                } else {
                    refused.push(format!("{}: {}", dotted(&here), fits::<T>(&candidate).unwrap_err()));
                }
            }
            (Err(e), Value::Array(items)) => {
                let mut prepared = root.clone();
                *slot(&mut prepared, &here) = Value::Array(Vec::new());
                if fits::<T>(&prepared).is_err() {
                    refused.push(format!("{}: {e}", dotted(&here)));
                    continue;
                }
                *root = prepared;
                for (i, item) in items.iter().enumerate() {
                    let mut next = root.clone();
                    if let Value::Array(list) = slot(&mut next, &here) {
                        list.push(item.clone());
                    }
                    match fits::<T>(&next) {
                        Ok(()) => *root = next,
                        Err(e) => refused.push(format!("{}[{i}]: {e}; entry skipped", dotted(&here))),
                    }
                }
            }
            (Err(e), _) => refused.push(format!("{}: {e}; default kept", dotted(&here))),
        }
    }
}

/// The schema node `s` stands for: a `$ref` resolved, an `Option<T>`'s
/// `anyOf [T, null]` unwrapped to `T`.
fn resolve<'a>(s: &'a Value, defs: &'a Value) -> &'a Value {
    if let Some(r) = s.get("$ref").and_then(Value::as_str)
        && let Some(name) = r.rsplit('/').next()
        && let Some(d) = defs.get(name)
    {
        return resolve(d, defs);
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        if let Some(Value::Array(alts)) = s.get(key)
            && let Some(first) = alts.iter().find(|a| a.get("type").and_then(Value::as_str) != Some("null"))
        {
            return resolve(first, defs);
        }
    }
    s
}

/// Keys in `doc` the schema does not declare, each with the nearest
/// declared key. Maps with open keys (`additionalProperties`) are not
/// checked below their own level.
#[must_use]
pub fn unknown_keys(schema: &Value, doc: &Value) -> Vec<String> {
    let defs = schema.get("definitions").cloned().unwrap_or(Value::Null);
    let mut out = Vec::new();
    walk(schema, &defs, doc, &mut Vec::new(), &mut out);
    out
}

fn walk(s: &Value, defs: &Value, doc: &Value, path: &mut Vec<String>, out: &mut Vec<String>) {
    let s = resolve(s, defs);
    match doc {
        Value::Object(map) => {
            let Some(props) = s.get("properties").and_then(Value::as_object) else { return };
            let known: Vec<&str> = props.keys().map(String::as_str).collect();
            for (k, v) in map {
                path.push(k.clone());
                if let Some(sub) = props.get(k) {
                    walk(sub, defs, v, path, out);
                } else {
                    let hint = shikumi::ConfigCoverage::did_you_mean(k, &known)
                        .map_or_else(String::new, |m| format!(" (did you mean {m}?)"));
                    out.push(format!("{}: unknown key, ignored{hint}", path.join(".")));
                }
                path.pop();
            }
        }
        Value::Array(items) => {
            if let Some(item) = s.get("items").filter(|i| i.is_object()) {
                for (i, v) in items.iter().enumerate() {
                    path.push(format!("[{i}]"));
                    walk(item, defs, v, path, out);
                    path.pop();
                }
            }
        }
        _ => {}
    }
}

/// Every leaf of `v` as `(dotted.path, value)`, lists as one leaf.
#[must_use]
pub fn leaves(v: &Value) -> Vec<(String, Value)> {
    fn go(v: &Value, path: &mut Vec<String>, out: &mut Vec<(String, Value)>) {
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, x) in m {
                    path.push(k.clone());
                    go(x, path, out);
                    path.pop();
                }
            }
            other => out.push((path.join("."), other.clone())),
        }
    }
    let mut out = Vec::new();
    go(v, &mut Vec::new(), &mut out);
    out
}

/// Whether the dotted `path` is set in `doc` (or under a set list).
#[must_use]
pub fn is_set(doc: &Value, path: &str) -> bool {
    let mut cur = doc;
    for k in path.split('.') {
        match cur.get(k) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, schemars::JsonSchema, Debug, Default, PartialEq)]
    #[serde(default)]
    struct Inner {
        rows: Option<u32>,
        name: Option<String>,
    }

    #[derive(Serialize, Deserialize, schemars::JsonSchema, Debug, Default, PartialEq)]
    #[serde(default)]
    struct Doc {
        a: Option<u32>,
        b: Option<String>,
        inner: Inner,
        list: Vec<Inner>,
    }

    #[test]
    fn a_bad_leaf_is_refused_and_its_siblings_load() {
        let doc = serde_json::json!({
            "a": "not a number",
            "b": "kept",
            "inner": {"rows": -3, "name": "kept too"},
            "list": [{"rows": 1}, {"rows": "x"}, {"rows": 3}]
        });
        let (d, refused) = salvage(&Doc::default(), &doc);
        assert_eq!(d.a, None);
        assert_eq!(d.b.as_deref(), Some("kept"));
        assert_eq!(d.inner, Inner { rows: None, name: Some("kept too".into()) });
        assert_eq!(d.list.iter().map(|i| i.rows).collect::<Vec<_>>(), [Some(1), Some(3)]);
        assert_eq!(refused.0.len(), 3, "{refused:?}");
        assert!(refused.0[0].starts_with("a: "));
        assert!(refused.0.iter().any(|r| r.starts_with("inner.rows: ")));
        assert!(refused.0.iter().any(|r| r.starts_with("list[1]: ")));
    }

    #[test]
    fn unknown_keys_are_named_with_the_nearest_known_one() {
        let schema = serde_json::to_value(schemars::schema_for!(Doc)).unwrap();
        let doc = serde_json::json!({"aa": 1, "inner": {"rowz": 2}, "list": [{"nme": "x"}]});
        let u = unknown_keys(&schema, &doc);
        assert_eq!(u.len(), 3, "{u:?}");
        assert!(u.iter().any(|s| s.starts_with("inner.rowz") && s.contains("rows")), "{u:?}");
        assert!(u.iter().any(|s| s.starts_with("list.[0].nme")), "{u:?}");
    }
}
