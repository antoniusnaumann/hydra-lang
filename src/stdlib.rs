//! Small standard modules. Operational failures use fs-style fallback overloads.
use std::io::{BufRead, IsTerminal, Read, Write};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::errors::Crash;
use crate::value::{
    boolean, deref, dict_entries, list_items, new_dict, new_list, sym, to_text, Native, Value,
};

pub(crate) fn string(text: impl AsRef<str>) -> Value {
    Value::Str(crate::text::Text::new(text.as_ref()))
}
pub(crate) fn text(value: &Value) -> Result<String, Crash> {
    match deref(value)? {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(Crash::new(format!(
            "expected a string, got {}",
            other.kind()
        ))),
    }
}
/// [`text`] without the copy: the string itself, shared.
pub(crate) fn shared_text(value: &Value) -> Result<crate::text::Text, Crash> {
    match value {
        Value::Str(s) => Ok(s.clone()),
        other => match deref(other)? {
            Value::Str(s) => Ok(s),
            other => Err(Crash::new(format!("expected a string, got {}", other.kind()))),
        },
    }
}
pub(crate) fn number(value: &Value) -> Result<f64, Crash> {
    let n = deref(value)?.as_num("standard module")?;
    if !n.is_finite() {
        return Err(Crash::new("expected a finite number"));
    }
    Ok(n)
}
pub(crate) fn duration(value: &Value) -> Result<Duration, Crash> {
    Duration::try_from_secs_f64(number(value)?)
        .map_err(|_| Crash::new("duration must be finite, nonnegative, and representable"))
}
pub(crate) fn success<T: From<Vec<Value>>>(value: Value) -> Result<T, Crash> {
    Ok(vec![value, Value::null()].into())
}
pub(crate) fn failure<T: From<Vec<Value>>>(
    fallback: Option<Value>,
    reason: &str,
    message: impl std::fmt::Display,
) -> Result<T, Crash> {
    match fallback {
        Some(value) => Ok(vec![value, Value::Sym(sym(reason))].into()),
        None => Err(Crash::new(message.to_string())),
    }
}
pub(crate) fn argument(args: &[Option<Value>], native: Native, name: &str) -> Option<Value> {
    native
        .info()
        .params
        .iter()
        .position(|(key, _)| *key == name)
        .and_then(|i| args.get(i))
        .cloned()
        .flatten()
}

/// Operations that must not occupy a scheduler worker while waiting.
pub(crate) fn blocking(native: Native) -> bool {
    native.module() == Some("http")
        || matches!(
            native,
            Native::TimeSleep
                | Native::IoRead
                | Native::IoReadOr
                | Native::IoReadLine
                | Native::IoReadLineOr
                | Native::IoLines
                | Native::IoLinesOr
                | Native::IoWrite
                | Native::IoFlush
        )
}

/// What a builtin answered: most answer one value, and building a vector for
/// it would be the costliest part of a cheap call.
pub(crate) enum Answer {
    One(Value),
    Many(Vec<Value>),
}

impl From<Vec<Value>> for Answer {
    fn from(values: Vec<Value>) -> Answer {
        Answer::Many(values)
    }
}

impl Answer {
    pub(crate) fn into_vec(self) -> Vec<Value> {
        match self {
            Answer::One(value) => vec![value],
            Answer::Many(values) => values,
        }
    }
}

pub(crate) fn call(
    native: Native,
    args: &[Option<Value>],
    script_args: &[String],
) -> Result<Answer, Crash> {
    answer(native, args, script_args)
}

fn answer(
    native: Native,
    args: &[Option<Value>],
    script_args: &[String],
) -> Result<Answer, Crash> {
    let arg = |name: &str| argument(args, native, name);
    let need = |name: &str| shared_text(&arg(name).unwrap_or_else(Value::null));
    let one = |v| Ok(Answer::One(v));
    match native {
        Native::EnvArgs => one(new_list(script_args.iter().cloned().map(string).collect())),
        Native::EnvPlatform => one(Value::Sym(sym(std::env::consts::OS))),
        Native::EnvGet | Native::EnvGetOr => {
            let name = need("name")?;
            if name.is_empty() || name.contains(['=', '\0']) {
                return Err(Crash::new("invalid environment variable name"));
            }
            match std::env::var(name.as_str()) {
                Ok(v) => success(string(v)),
                Err(e) => failure(
                    arg("fallback"),
                    if matches!(e, std::env::VarError::NotPresent) {
                        "not_found"
                    } else {
                        "encoding"
                    },
                    format!("env::get({name}): {e}"),
                ),
            }
        }
        Native::EnvAll => {
            let mut vars: Vec<_> = std::env::vars_os()
                .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                .collect();
            vars.sort();
            one(new_dict(
                vars.into_iter()
                    .map(|(k, v)| (sym(&k), string(v)))
                    .collect(),
            ))
        }
        Native::TextSplit => {
            let value = need("text")?;
            let separator = arg("separator")
                .map(|v| deref(&v))
                .transpose()?
                .unwrap_or_else(Value::null);
            let parts: Vec<Value> = if matches!(&separator, Value::Sym(s) if s.name() == "null") {
                value.split_whitespace().map(string).collect()
            } else {
                let separator = shared_text(&separator)?;
                let mut chars = separator.chars();
                match (chars.next(), chars.next()) {
                    (None, _) => value.chars().map(|c| string(c.to_string())).collect(),
                    // One character is the usual separator, and searching
                    // for a character is cheaper than for a string.
                    (Some(c), None) => {
                        let mut parts = Vec::with_capacity(8);
                        parts.extend(value.split(c).map(string));
                        parts
                    }
                    _ => value.split(separator.as_str()).map(string).collect(),
                }
            };
            one(new_list(parts))
        }
        Native::TextJoin => {
            let Value::List(list) = deref(&arg("parts").unwrap_or_else(Value::null))? else {
                return Err(Crash::new("text::join expects a list of strings"));
            };
            let separator = match arg("separator") {
                Some(separator) => shared_text(&separator)?,
                None => crate::text::Text::new(""),
            };
            // The parts are taken as handles, and the text copied once, into
            // a string sized for it. No lock is held while a `&` among them
            // is followed.
            let parts = list_items(&list);
            let mut length = separator.len() * parts.len().saturating_sub(1);
            for part in &parts {
                length += match part {
                    Value::Str(s) => s.len(),
                    _ => 0,
                };
            }
            let mut joined = crate::text::Text::empty(length);
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    joined.push_str(&separator);
                }
                match part {
                    Value::Str(s) => joined.push_str(s),
                    other => joined.push_str(&shared_text(other)?),
                }
            }
            one(Value::Str(joined))
        }
        Native::TextTrim => one(string(need("text")?.trim())),
        Native::TextLower => one(string(need("text")?.to_lowercase())),
        Native::TextUpper => one(string(need("text")?.to_uppercase())),
        Native::TextIsWhitespace => {
            let text = need("text")?;
            one(boolean(!text.is_empty() && text.chars().all(char::is_whitespace)))
        }
        Native::TextReplace => one(string(need("text")?.replace(need("from")?.as_str(), &need("to")?))),
        Native::TextFind => {
            let text = need("text")?;
            one(text
                .find(need("needle")?.as_str())
                .map(|i| Value::Num(text[..i].chars().count() as f64))
                .unwrap_or_else(Value::null))
        }
        Native::TextStartsWith => one(boolean(need("text")?.starts_with(need("prefix")?.as_str()))),
        Native::TextEndsWith => one(boolean(need("text")?.ends_with(need("suffix")?.as_str()))),
        Native::JsonParse | Native::JsonParseOr => {
            let source = need("text")?;
            // A lone number is common enough — a field of a line of text —
            // to read without building a JSON tree for it.
            if let Some(n) = json_number(&source) {
                return success(Value::Num(n));
            }
            match serde_json::from_str::<Json>(&source) {
                Ok(Json(v)) => success(v),
                Err(e) => failure(arg("fallback"), "invalid", format!("json::parse: {e}")),
            }
        }
        Native::JsonStringify => {
            let value = arg("value").unwrap_or_else(Value::null);
            let v = JsonOut { value: &value, depth: 0 };
            let pretty = arg("pretty")
                .map(|v| deref(&v).map(|v| v.truthy()))
                .transpose()?
                .unwrap_or(false);
            let s = if pretty {
                serde_json::to_string_pretty(&v)
            } else {
                serde_json::to_string(&v)
            };
            one(string(s.map_err(|e| Crash::new(e.to_string()))?))
        }
        Native::TimeNow => {
            let n = match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_secs_f64(),
                Err(e) => -e.duration().as_secs_f64(),
            };
            one(Value::Num(n))
        }
        Native::TimeMonotonic => {
            static START: OnceLock<Instant> = OnceLock::new();
            one(Value::Num(
                START.get_or_init(Instant::now).elapsed().as_secs_f64(),
            ))
        }
        Native::TimeSleep => {
            std::thread::sleep(duration(&arg("seconds").unwrap_or_else(Value::null))?);
            one(Value::null())
        }
        Native::TimeParse | Native::TimeParseOr => {
            match chrono::DateTime::parse_from_rfc3339(&need("text")?) {
                Ok(t) => success(Value::Num(
                    t.timestamp() as f64 + t.timestamp_subsec_nanos() as f64 / 1e9,
                )),
                Err(e) => failure(arg("fallback"), "invalid", format!("time::parse: {e}")),
            }
        }
        Native::TimeFormat => {
            let t = number(&arg("timestamp").unwrap_or_else(Value::null))?;
            let secs = t.floor();
            let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(
                secs as i64,
                ((t - secs) * 1e9) as u32,
            )
            .ok_or_else(|| Crash::new("timestamp outside supported date range"))?;
            let fmt = arg("format")
                .map(|v| text(&v))
                .transpose()?
                .unwrap_or_else(|| "%+".into());
            let mut rendered = String::new();
            use std::fmt::Write;
            write!(&mut rendered, "{}", dt.format(&fmt))
                .map_err(|_| Crash::new("invalid time format"))?;
            one(string(rendered))
        }
        Native::IoRead
        | Native::IoReadOr
        | Native::IoReadLine
        | Native::IoReadLineOr
        | Native::IoLines
        | Native::IoLinesOr => {
            stream(arg("stream"), "stdin", &["stdin"])?;
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let mut buffer = String::new();
            let line = matches!(native, Native::IoReadLine | Native::IoReadLineOr);
            let result = if line {
                input.read_line(&mut buffer)
            } else {
                input.read_to_string(&mut buffer)
            };
            match result {
                Ok(0) if line => success(Value::null()),
                Ok(_) => {
                    if line {
                        if buffer.ends_with('\n') {
                            buffer.pop();
                            if buffer.ends_with('\r') {
                                buffer.pop();
                            }
                        }
                        success(string(buffer))
                    } else if matches!(native, Native::IoLines | Native::IoLinesOr) {
                        success(new_list(buffer.lines().map(string).collect()))
                    } else {
                        success(string(buffer))
                    }
                }
                Err(e) => failure(
                    arg("fallback"),
                    if e.kind() == std::io::ErrorKind::InvalidData {
                        "encoding"
                    } else {
                        "io"
                    },
                    format!("io::{}: {e}", native.name()),
                ),
            }
        }
        Native::IoWrite | Native::IoFlush => {
            let target = stream(arg("stream"), "stdout", &["stdout", "stderr"])?;
            let value = arg("value")
                .map(|v| deref(&v).map(|v| to_text(&v)))
                .transpose()?
                .unwrap_or_default();
            let write = |out: &mut dyn Write| -> std::io::Result<()> {
                if native == Native::IoWrite {
                    out.write_all(value.as_bytes())?;
                }
                out.flush()
            };
            let result = if target == "stdout" {
                write(&mut std::io::stdout().lock())
            } else {
                write(&mut std::io::stderr().lock())
            };
            result.map_err(|e| Crash::new(format!("io::{}: {e}", native.name())))?;
            one(if native == Native::IoWrite {
                Value::Num(value.len() as f64)
            } else {
                Value::null()
            })
        }
        Native::IoIsTerminal => {
            let target = stream(arg("stream"), "stdout", &["stdin", "stdout", "stderr"])?;
            one(boolean(match target.as_str() {
                "stdin" => std::io::stdin().is_terminal(),
                "stderr" => std::io::stderr().is_terminal(),
                _ => std::io::stdout().is_terminal(),
            }))
        }
        other if other.module() == Some("http") => crate::http::call(other, args).map(Answer::Many),
        _ => unreachable!("not a standard module native: {native:?}"),
    }
}

fn stream(value: Option<Value>, default: &str, allowed: &[&str]) -> Result<String, Crash> {
    let name = match value {
        None => default.to_owned(),
        Some(v) => match deref(&v)? {
            Value::Sym(s) => s.name().to_owned(),
            _ => return Err(Crash::new("stream must be :stdin, :stdout, or :stderr")),
        },
    };
    if !allowed.contains(&name.as_str()) {
        return Err(Crash::new(format!(
            "unsupported stream :{name}; expected {}",
            allowed.join(" or ")
        )));
    }
    Ok(name)
}

/// `text` as a number, if it is exactly one JSON number — optionally with
/// whitespace around it — that a float holds finitely.
fn json_number(text: &str) -> Option<f64> {
    let text = text.trim_matches([' ', '\t', '\n', '\r']);
    let bytes = text.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    if bytes.first() == Some(&b'-') {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return None,
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return None;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }
    text.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// A Hydra value read straight from JSON text, with no JSON tree between.
struct Json(Value);

impl<'de> serde::Deserialize<'de> for Json {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> serde::de::Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, b: bool) -> Result<Json, E> {
        Ok(Json(boolean(b)))
    }

    fn visit_i64<E>(self, n: i64) -> Result<Json, E> {
        Ok(Json(Value::Num(n as f64)))
    }

    fn visit_u64<E>(self, n: u64) -> Result<Json, E> {
        Ok(Json(Value::Num(n as f64)))
    }

    fn visit_f64<E: serde::de::Error>(self, n: f64) -> Result<Json, E> {
        if !n.is_finite() {
            return Err(E::custom("JSON number outside Hydra's numeric range"));
        }
        Ok(Json(Value::Num(n)))
    }

    fn visit_str<E>(self, s: &str) -> Result<Json, E> {
        Ok(Json(string(s)))
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json(Value::null()))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(Json(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json(new_list(items)))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
            let Json(value) = map.next_value()?;
            entries.push((sym(&key), value));
        }
        // A key given twice keeps its first place and its last value, as a
        // JSON object read into an ordered map does.
        Ok(Json(new_dict(entries)))
    }
}

/// A Hydra value written straight out as JSON.
struct JsonOut<'a> {
    value: &'a Value,
    depth: usize,
}

impl serde::Serialize for JsonOut<'_> {
    fn serialize<S: serde::Serializer>(&self, out: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeMap, SerializeSeq};
        if self.depth > 128 {
            return Err(S::Error::custom("json::stringify: value is cyclic or nested too deeply"));
        }
        let value = deref(self.value).map_err(|crash| S::Error::custom(crash.into_message()))?;
        let depth = self.depth + 1;
        match &value {
            Value::Num(n) if n.is_finite() => out.serialize_f64(*n),
            Value::Num(_) => Err(S::Error::custom("JSON cannot represent non-finite numbers")),
            Value::Str(s) => out.serialize_str(s),
            Value::Sym(s) => match s.name() {
                "null" => out.serialize_unit(),
                "true" => out.serialize_bool(true),
                "false" => out.serialize_bool(false),
                name => out.serialize_str(name),
            },
            Value::List(list) => {
                let items = list_items(list);
                let mut seq = out.serialize_seq(Some(items.len()))?;
                for item in &items {
                    seq.serialize_element(&JsonOut { value: item, depth })?;
                }
                seq.end()
            }
            Value::Dict(dict) => {
                let entries = dict_entries(dict);
                let mut map = out.serialize_map(Some(entries.len()))?;
                for (key, item) in &entries {
                    map.serialize_entry(key.name(), &JsonOut { value: item, depth })?;
                }
                map.end()
            }
            _ => Err(S::Error::custom("JSON cannot represent functions or unresolved references")),
        }
    }
}
