//! How a view's JSON changes between two revisions. A view is a JSON object; a field that
//! changed is replaced whole, except the long lists, which go entry by entry: a thread's
//! messages by `seq` (and its pages' marks by page), the sidebar's rows by thread id. The server makes the ops with `diff`
//! and the client applies them with `apply`; applied to the old value they give exactly the
//! new one.

use crate::ViewKey;
use crate::wire::Op;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

/// The list fields sent entry by entry, and the path to each entry's key.
pub fn keyed(view: &ViewKey, field: &str) -> Option<&'static [&'static str]> {
    match (view, field) {
        (ViewKey::Thread(_), "messages") => Some(&["seq"]),
        (ViewKey::Thread(_), "pages") => Some(&["page"]),
        (ViewKey::Sidebar, "threads") => Some(&["thread", "id"]),
        (ViewKey::Devices, "devices") => Some(&["id"]),
        (ViewKey::Schedules, "schedules") => Some(&["id"]),
        _ => None,
    }
}

/// The ops that turn `old` into `new`, or None when only the whole will do (not objects).
/// Empty when they're equal.
pub fn diff(view: &ViewKey, old: &Value, new: &Value) -> Option<Vec<Op>> {
    let (Value::Object(old), Value::Object(new)) = (old, new) else {
        return None;
    };
    let mut ops = Vec::new();
    for (field, v) in new {
        let was = old.get(field);
        if was == Some(v) {
            continue;
        }
        let entries = match (keyed(view, field), was, v) {
            (Some(path), Some(Value::Array(a)), Value::Array(b)) => diff_list(field, path, a, b),
            _ => None,
        };
        match entries {
            Some(mut list) => ops.append(&mut list),
            None => ops.push(Op::Set {
                field: field.clone(),
                v: v.clone(),
            }),
        }
    }
    for field in old.keys() {
        if !new.contains_key(field) {
            ops.push(Op::Unset {
                field: field.clone(),
            });
        }
    }
    Some(ops)
}

/// Apply `ops` to `data` (the value at the delta's base). An error means the client doesn't
/// hold what the server thinks it does: it should ask for the view whole.
pub fn apply(view: &ViewKey, data: &mut Value, ops: &[Op]) -> Result<(), String> {
    let Value::Object(fields) = data else {
        return Err("the view isn't an object".into());
    };
    for op in ops {
        match op {
            Op::Set { field, v } => {
                fields.insert(field.clone(), v.clone());
            }
            Op::Unset { field } => {
                fields.remove(field);
            }
            Op::Put { field, v } => {
                let (path, list) = list_mut(view, fields, field)?;
                let key = key_of(v, path)
                    .ok_or_else(|| format!("an entry of {field} without its key"))?;
                match list.iter().position(|e| key_of(e, path) == Some(key)) {
                    Some(i) => list[i] = v.clone(),
                    None => list.push(v.clone()),
                }
            }
            Op::Del { field, keys } => {
                let (path, list) = list_mut(view, fields, field)?;
                for key in keys {
                    let i = list
                        .iter()
                        .position(|e| key_of(e, path) == Some(key))
                        .ok_or_else(|| format!("{field} has no {key} to remove"))?;
                    list.remove(i);
                }
            }
            Op::Order { field, keys } => {
                let (path, list) = list_mut(view, fields, field)?;
                if keys.len() != list.len() {
                    return Err(format!(
                        "{field}'s order names {} entries, not {}",
                        keys.len(),
                        list.len()
                    ));
                }
                let mut by_key: HashMap<String, Value> = HashMap::new();
                for e in list.drain(..) {
                    let k = key_of(&e, path)
                        .map(text)
                        .ok_or_else(|| format!("an entry of {field} without its key"))?;
                    by_key.insert(k, e);
                }
                for key in keys {
                    let e = by_key
                        .remove(&text(key))
                        .ok_or_else(|| format!("{field}'s order names {key}, which it hasn't"))?;
                    list.push(e);
                }
            }
            Op::Unknown => {}
        }
    }
    Ok(())
}

fn list_mut<'a>(
    view: &ViewKey,
    fields: &'a mut Map<String, Value>,
    field: &str,
) -> Result<(&'static [&'static str], &'a mut Vec<Value>), String> {
    let path = keyed(view, field).ok_or_else(|| format!("{field} isn't a keyed list"))?;
    match fields.get_mut(field) {
        Some(Value::Array(list)) => Ok((path, list)),
        _ => Err(format!("{field} isn't a list here")),
    }
}

/// The puts, dels and (when they don't leave the order right) order for one keyed list. None
/// if an entry has no key or two share one: then the list goes whole.
fn diff_list(field: &str, path: &[&str], old: &[Value], new: &[Value]) -> Option<Vec<Op>> {
    let old_keys = keys(old, path)?;
    let new_keys = keys(new, path)?;
    let old_at: HashMap<&str, usize> = old_keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let new_set: HashSet<&str> = new_keys.iter().map(String::as_str).collect();
    let mut ops = Vec::new();
    let gone: Vec<Value> = old
        .iter()
        .zip(&old_keys)
        .filter(|(_, k)| !new_set.contains(k.as_str()))
        .map(|(e, _)| key_of(e, path).cloned().unwrap_or(Value::Null))
        .collect();
    if !gone.is_empty() {
        ops.push(Op::Del {
            field: field.to_string(),
            keys: gone,
        });
    }
    // What the client's list will be after the dels and puts: the old order, less what went,
    // then what's new, appended in the new order.
    let mut after: Vec<&str> = old_keys
        .iter()
        .map(String::as_str)
        .filter(|k| new_set.contains(k))
        .collect();
    for (e, k) in new.iter().zip(&new_keys) {
        match old_at.get(k.as_str()) {
            Some(&i) if old[i] == *e => {}
            Some(_) => ops.push(Op::Put {
                field: field.to_string(),
                v: e.clone(),
            }),
            None => {
                ops.push(Op::Put {
                    field: field.to_string(),
                    v: e.clone(),
                });
                after.push(k);
            }
        }
    }
    if after.iter().zip(&new_keys).any(|(a, b)| *a != b.as_str()) {
        ops.push(Op::Order {
            field: field.to_string(),
            keys: new
                .iter()
                .map(|e| key_of(e, path).cloned().unwrap_or(Value::Null))
                .collect(),
        });
    }
    Some(ops)
}

/// Each entry's key as text, if all have one and no two share it.
fn keys(list: &[Value], path: &[&str]) -> Option<Vec<String>> {
    let keys: Vec<String> = list
        .iter()
        .map(|e| key_of(e, path).map(text))
        .collect::<Option<_>>()?;
    let unique: HashSet<&String> = keys.iter().collect();
    (unique.len() == keys.len()).then_some(keys)
}

fn key_of<'a>(entry: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut v = entry;
    for p in path {
        v = v.get(p)?;
    }
    (!v.is_null()).then_some(v)
}

fn text(v: &Value) -> String {
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check(view: &ViewKey, old: Value, new: Value) -> Vec<Op> {
        let ops = diff(view, &old, &new).unwrap();
        let mut got = old.clone();
        apply(view, &mut got, &ops).unwrap();
        assert_eq!(got, new, "ops {ops:?}");
        // And through JSON, as they travel.
        let ops: Vec<Op> = serde_json::from_str(&serde_json::to_string(&ops).unwrap()).unwrap();
        let mut got = old;
        apply(view, &mut got, &ops).unwrap();
        assert_eq!(got, new);
        ops
    }

    fn msg(seq: i64, text: &str) -> Value {
        json!({"seq": seq, "kind": "text", "text": text})
    }

    fn row(id: &str, title: &str) -> Value {
        json!({"thread": {"id": id, "title": title}, "activity": "idle"})
    }

    #[test]
    fn messages_go_by_seq_and_other_fields_whole() {
        let t = ViewKey::Thread("t1".into());
        let old = json!({"id": "t1", "messages": [msg(0, "a"), msg(1, "b")], "start_error": null});
        // One appended, one changed in place, a field replaced.
        let ops = check(
            &t,
            old.clone(),
            json!({"id": "t1", "messages": [msg(0, "a"), msg(1, "b!"), msg(2, "c")], "start_error": "boom"}),
        );
        assert_eq!(ops.len(), 3);
        assert!(ops.iter().all(|o| !matches!(o, Op::Order { .. })));
        // A partial gone and its final in its place; the tail trimmed.
        check(
            &t,
            json!({"id": "t1", "messages": [msg(0, "a"), msg(1, "b"), msg(2, "c")]}),
            json!({"id": "t1", "messages": [msg(0, "a"), msg(1, "B")]}),
        );
        // Emptied, and filled from empty.
        check(
            &t,
            old.clone(),
            json!({"id": "t1", "messages": [], "start_error": null}),
        );
        check(
            &t,
            json!({"id": "t1", "messages": []}),
            json!({"id": "t1", "messages": [msg(0, "a")]}),
        );
        // Equal: nothing.
        assert!(diff(&t, &old, &old).unwrap().is_empty());
        // A field gone.
        check(&t, old, json!({"id": "t1", "messages": []}));
    }

    #[test]
    fn rows_go_by_thread_id_and_keep_their_order() {
        let s = ViewKey::Sidebar;
        let old = json!({"threads": [row("a", "A"), row("b", "B"), row("c", "C")], "waiting": []});
        // A row moves to the top: an order op.
        let ops = check(
            &s,
            old.clone(),
            json!({"threads": [row("c", "C!"), row("a", "A"), row("b", "B")], "waiting": []}),
        );
        assert!(ops.iter().any(|o| matches!(o, Op::Order { .. })));
        // A new row at the top, one gone.
        check(
            &s,
            old.clone(),
            json!({"threads": [row("d", "D"), row("a", "A"), row("c", "C")], "waiting": ["a"]}),
        );
        // A new row last needs no order.
        let ops = check(
            &s,
            old.clone(),
            json!({"threads": [row("a", "A"), row("b", "B"), row("c", "C"), row("d", "D")], "waiting": []}),
        );
        assert!(!ops.iter().any(|o| matches!(o, Op::Order { .. })));
        // Two rows with one id: the list goes whole.
        let ops = check(
            &s,
            old,
            json!({"threads": [row("a", "A"), row("a", "A2")], "waiting": []}),
        );
        assert!(matches!(&ops[..], [Op::Set { .. }]));
    }

    #[test]
    fn what_cant_be_applied_says_so() {
        let t = ViewKey::Thread("t1".into());
        let mut v = json!({"messages": [msg(0, "a")]});
        assert!(
            apply(
                &t,
                &mut v,
                &[Op::Del {
                    field: "messages".into(),
                    keys: vec![json!(7)]
                }]
            )
            .is_err()
        );
        assert!(
            apply(
                &t,
                &mut v,
                &[Op::Order {
                    field: "messages".into(),
                    keys: vec![json!(0), json!(1)]
                }]
            )
            .is_err()
        );
        assert!(
            apply(
                &t,
                &mut v,
                &[Op::Put {
                    field: "sent".into(),
                    v: json!({})
                }]
            )
            .is_err()
        );
        let mut not_object = json!(null);
        assert!(apply(&t, &mut not_object, &[]).is_err());
        // Not objects: only a snapshot will do.
        assert!(diff(&ViewKey::Catalog, &json!(null), &json!({"providers": []})).is_none());
        // An op from a newer server is skipped.
        let op: Op = serde_json::from_str(r#"{"op":"splice","field":"messages","at":3}"#).unwrap();
        assert_eq!(op, Op::Unknown);
        apply(&t, &mut v, &[op]).unwrap();
    }

    /// Random lists, changed at random: the ops always rebuild the new one.
    #[test]
    fn random_changes_round_trip() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let s = ViewKey::Sidebar;
        let mut ids: Vec<u64> = (0..6).collect();
        let mut old =
            json!({"threads": ids.iter().map(|i| row(&i.to_string(), "x")).collect::<Vec<_>>()});
        for round in 0..500 {
            // Drop some, add some, move some, retitle some.
            ids.retain(|_| next(5) != 0);
            for _ in 0..next(3) {
                ids.insert(
                    next(ids.len() as u64 + 1) as usize,
                    100 + round * 3 + next(3),
                );
            }
            ids.dedup();
            let mut seen = HashSet::new();
            ids.retain(|i| seen.insert(*i));
            if ids.len() > 1 && next(2) == 0 {
                let a = next(ids.len() as u64) as usize;
                let b = next(ids.len() as u64) as usize;
                ids.swap(a, b);
            }
            let new = json!({"threads": ids.iter().map(|i| row(&i.to_string(), if next(4) == 0 { "y" } else { "x" })).collect::<Vec<_>>()});
            check(&s, old, new.clone());
            old = new;
        }
    }
}
