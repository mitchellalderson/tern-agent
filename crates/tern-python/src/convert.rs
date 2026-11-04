//! Values between JSON (tool inputs and results) and Monty.

use monty_types::{CallArgs, MontyObject, ObjectRef};
use serde_json::{Map, Number, Value};
use tern_core::types::ToolSpec;

pub fn to_monty(v: &Value) -> MontyObject {
    match v {
        Value::Null => MontyObject::none(),
        Value::Bool(b) => MontyObject::bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => MontyObject::int(i),
            None => MontyObject::float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => MontyObject::string(s.clone()),
        Value::Array(items) => MontyObject::list(items.iter().map(to_monty)),
        Value::Object(map) => MontyObject::dict(
            map.iter()
                .map(|(k, v)| (MontyObject::string(k.clone()), to_monty(v))),
        ),
    }
}

pub fn to_json(v: &MontyObject) -> Value {
    ref_to_json(&v.as_ref())
}

/// Containers map to arrays and objects; anything without a JSON form
/// (a date, a dataclass, a big int) to its `repr`.
fn ref_to_json(v: &ObjectRef<'_>) -> Value {
    match v.type_name() {
        "NoneType" => return Value::Null,
        "bool" => return v.as_bool().map_or(Value::Null, Value::Bool),
        "int" => {
            if let Some(i) = v.as_int() {
                return Value::from(i);
            }
        }
        "float" => {
            if let Some(n) = v.as_float().and_then(Number::from_f64) {
                return Value::Number(n);
            }
        }
        "str" => {
            return v
                .as_str()
                .map_or(Value::Null, |s| Value::String(s.to_owned()));
        }
        "dict" => {
            if let Some(pairs) = v.pairs() {
                let mut map = Map::new();
                for (k, val) in pairs {
                    let key = k.as_str().map_or_else(|| k.py_repr(), str::to_owned);
                    map.insert(key, ref_to_json(&val));
                }
                return Value::Object(map);
            }
        }
        _ => {
            if let Some(items) = v.items() {
                return Value::Array(items.iter().map(ref_to_json).collect());
            }
        }
    }
    Value::String(v.py_repr())
}

/// A tool's result for the script: parsed JSON when it is JSON (Luau
/// connectors return tables), else the text.
pub fn tool_output(content: &str) -> MontyObject {
    match serde_json::from_str::<Value>(content) {
        Ok(v @ (Value::Object(_) | Value::Array(_))) => to_monty(&v),
        _ => MontyObject::string(content.to_owned()),
    }
}

/// The tool input for a call: keyword arguments by name, positional ones
/// filling the schema's `required` properties in order, or one positional
/// dict as the whole input.
pub fn call_input(args: &CallArgs, spec: &ToolSpec) -> Result<Value, String> {
    let name = &spec.name;
    let schema = &spec.input_schema;
    let props = schema["properties"].as_object();
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let positional: Vec<ObjectRef<'_>> = args.args().collect();
    let mut input = Map::new();
    if positional.len() == 1 && args.kwargs().len() == 0 && positional[0].type_name() == "dict" {
        // `tool({"a": 1})`, unless the only parameter is itself an object.
        let single_object =
            required.len() == 1 && props.is_some_and(|p| p[required[0]]["type"] == "object");
        if !single_object {
            return Ok(ref_to_json(&positional[0]));
        }
    }
    if positional.len() > required.len() {
        return Err(format!(
            "{name}() takes {} positional argument(s) ({}); pass the others by keyword",
            required.len(),
            required.join(", ")
        ));
    }
    for (value, key) in positional.iter().zip(&required) {
        input.insert((*key).to_owned(), ref_to_json(value));
    }
    for (key, value) in args.kwargs() {
        let Some(key) = key.as_str() else {
            return Err(format!("{name}(): keyword names must be strings"));
        };
        if let Some(p) = props
            && !p.contains_key(key)
        {
            let known: Vec<&str> = p.keys().map(String::as_str).collect();
            return Err(format!(
                "{name}() got an unexpected keyword argument '{key}'; it takes: {}",
                known.join(", ")
            ));
        }
        input.insert(key.to_owned(), ref_to_json(&value));
    }
    let missing: Vec<&str> = required
        .iter()
        .filter(|k| !input.contains_key(**k))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{name}() is missing required argument(s): {}",
            missing.join(", ")
        ));
    }
    Ok(Value::Object(input))
}
