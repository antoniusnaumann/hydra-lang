//! Small standard modules. Operational failures use fs-style fallback overloads.
use std::io::{BufRead, IsTerminal, Read, Write};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::errors::Crash;
use crate::value::{
    boolean, deref, dict_entries, list_items, new_dict, new_list, sym, to_text, Native, Value,
};

pub(crate) fn string(text: impl Into<String>) -> Value {
    Value::Str(text.into().into())
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
pub(crate) fn success(value: Value) -> Result<Vec<Value>, Crash> {
    Ok(vec![value, Value::null()])
}
pub(crate) fn failure(
    fallback: Option<Value>,
    reason: &str,
    message: impl std::fmt::Display,
) -> Result<Vec<Value>, Crash> {
    match fallback {
        Some(value) => Ok(vec![value, Value::Sym(sym(reason))]),
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

pub(crate) fn call(
    native: Native,
    args: &[Option<Value>],
    script_args: &[String],
) -> Result<Vec<Value>, Crash> {
    let arg = |name: &str| argument(args, native, name);
    let need = |name: &str| text(&arg(name).unwrap_or_else(Value::null));
    let one = |v| Ok(vec![v]);
    match native {
        Native::EnvArgs => one(new_list(script_args.iter().cloned().map(string).collect())),
        Native::EnvPlatform => one(Value::Sym(sym(std::env::consts::OS))),
        Native::EnvGet | Native::EnvGetOr => {
            let name = need("name")?;
            if name.is_empty() || name.contains(['=', '\0']) {
                return Err(Crash::new("invalid environment variable name"));
            }
            match std::env::var(&name) {
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
            let parts: Vec<String> = if matches!(&separator, Value::Sym(s) if s.name() == "null") {
                value.split_whitespace().map(str::to_owned).collect()
            } else {
                let separator = text(&separator)?;
                if separator.is_empty() {
                    value.chars().map(|c| c.to_string()).collect()
                } else {
                    value.split(&separator).map(str::to_owned).collect()
                }
            };
            one(new_list(parts.into_iter().map(string).collect()))
        }
        Native::TextJoin => {
            let Value::List(list) = deref(&arg("parts").unwrap_or_else(Value::null))? else {
                return Err(Crash::new("text::join expects a list of strings"));
            };
            let parts: Result<Vec<_>, _> = list_items(&list).iter().map(text).collect();
            let separator = arg("separator")
                .map(|v| text(&v))
                .transpose()?
                .unwrap_or_default();
            one(string(parts?.join(&separator)))
        }
        Native::TextTrim => one(string(need("text")?.trim())),
        Native::TextLower => one(string(need("text")?.to_lowercase())),
        Native::TextUpper => one(string(need("text")?.to_uppercase())),
        Native::TextReplace => one(string(need("text")?.replace(&need("from")?, &need("to")?))),
        Native::TextFind => {
            let text = need("text")?;
            one(text
                .find(&need("needle")?)
                .map(|i| Value::Num(text[..i].chars().count() as f64))
                .unwrap_or_else(Value::null))
        }
        Native::TextStartsWith => one(boolean(need("text")?.starts_with(&need("prefix")?))),
        Native::TextEndsWith => one(boolean(need("text")?.ends_with(&need("suffix")?))),
        Native::JsonParse | Native::JsonParseOr => {
            match serde_json::from_str::<serde_json::Value>(&need("text")?) {
                Ok(v) => match from_json(v) {
                    Ok(v) => success(v),
                    Err(e) => failure(arg("fallback"), "invalid", e),
                },
                Err(e) => failure(arg("fallback"), "invalid", format!("json::parse: {e}")),
            }
        }
        Native::JsonStringify => {
            let v = to_json(&arg("value").unwrap_or_else(Value::null), 0)?;
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
        other if other.module() == Some("http") => crate::http::call(other, args),
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

fn from_json(v: serde_json::Value) -> Result<Value, Crash> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::null(),
        J::Bool(b) => boolean(b),
        J::String(s) => string(s),
        J::Number(n) => Value::Num(
            n.as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| Crash::new("JSON number outside Hydra's numeric range"))?,
        ),
        J::Array(v) => new_list(v.into_iter().map(from_json).collect::<Result<_, _>>()?),
        J::Object(v) => new_dict(
            v.into_iter()
                .map(|(k, v)| Ok((sym(&k), from_json(v)?)))
                .collect::<Result<_, Crash>>()?,
        ),
    })
}

fn to_json(value: &Value, depth: usize) -> Result<serde_json::Value, Crash> {
    use serde_json::Value as J;
    if depth > 128 {
        return Err(Crash::new(
            "json::stringify: value is cyclic or nested too deeply",
        ));
    }
    Ok(match deref(value)? {
        Value::Num(n) => J::Number(
            serde_json::Number::from_f64(n)
                .ok_or_else(|| Crash::new("JSON cannot represent non-finite numbers"))?,
        ),
        Value::Str(s) => J::String(s.to_string()),
        Value::Sym(s) => match s.name() {
            "null" => J::Null,
            "true" => J::Bool(true),
            "false" => J::Bool(false),
            name => J::String(name.into()),
        },
        Value::List(v) => J::Array(
            list_items(&v)
                .iter()
                .map(|v| to_json(v, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        Value::Dict(v) => J::Object(
            dict_entries(&v)
                .into_iter()
                .map(|(k, v)| Ok((k.name().into(), to_json(&v, depth + 1)?)))
                .collect::<Result<_, Crash>>()?,
        ),
        _ => {
            return Err(Crash::new(
                "JSON cannot represent functions or unresolved references",
            ))
        }
    })
}
