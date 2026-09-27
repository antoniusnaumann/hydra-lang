//! Argparse-style builders backed by ordinary copy-on-write dictionaries.
use crate::errors::Crash;
use crate::stdlib::{argument, number, string, text};
use crate::value::{
    boolean, copy_value, deep_equal, deref, dict_entries, list_items, new_dict, new_list,
    read_place, sym, write_place, Native, Value,
};

pub(crate) enum Outcome {
    Values(Vec<Value>),
    Exit { code: u8, message: String },
}
fn field(value: &Value, name: &str) -> Result<Value, Crash> {
    let Value::Dict(d) = deref(value)? else {
        return Err(Crash::new(
            "cli parser and argument specifications must be dicts",
        ));
    };
    let found = d
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&sym(name))
        .map(copy_value);
    Ok(found.unwrap_or_else(Value::null))
}
fn put(value: &mut Value, name: &str, field: Value) -> Result<(), Crash> {
    let Value::Dict(d) = deref(value)? else {
        return Err(Crash::new("expected a parser dict"));
    };
    let mut entries = dict_entries(&d)
        .into_iter()
        .map(|(k, v)| (k, copy_value(&v)))
        .collect::<Vec<_>>();
    if let Some((_, v)) = entries.iter_mut().find(|(k, _)| k.name() == name) {
        *v = field;
    } else {
        entries.push((sym(name), field));
    }
    *value = new_dict(entries);
    Ok(())
}
fn null(v: &Value) -> bool {
    matches!(v,Value::Sym(s) if s.name()=="null")
}
fn list(v: &Value) -> Result<Vec<Value>, Crash> {
    match deref(v)? {
        Value::List(v) => Ok(list_items(&v).iter().map(copy_value).collect()),
        _ => Err(Crash::new("expected a list")),
    }
}
fn strings(v: &Value) -> Result<Vec<String>, Crash> {
    list(v)?.iter().map(text).collect()
}
fn name(v: &Value) -> Result<String, Crash> {
    match deref(v)? {
        Value::Sym(s) => Ok(s.name().into()),
        v => text(&v),
    }
}
fn parser(v: &Value) -> Result<(), Crash> {
    if field(v, "_kind")?.eq_symbol("cli_parser") {
        Ok(())
    } else {
        Err(Crash::new("expected a cli::parser value"))
    }
}
// Keep tag matching local rather than introducing a language-wide type.
trait Symbol {
    fn eq_symbol(&self, s: &str) -> bool;
}
impl Symbol for Value {
    fn eq_symbol(&self, s: &str) -> bool {
        matches!(self,Value::Sym(v) if v.name()==s)
    }
}

#[derive(Clone)]
struct Arg {
    names: Vec<String>,
    dest: String,
    help: String,
    kind: String,
    action: String,
    default: Value,
    required: bool,
    nargs: Nargs,
    choices: Option<Vec<Value>>,
    metavar: String,
    constant: Value,
    version: String,
}
#[derive(Clone, Copy)]
enum Nargs {
    One,
    Fixed(usize),
    Optional,
    Star,
    Plus,
    Zero,
}
impl Nargs {
    fn min(self) -> usize {
        match self {
            Self::One | Self::Plus => 1,
            Self::Fixed(n) => n,
            _ => 0,
        }
    }
    fn multiple(self) -> bool {
        matches!(self, Self::Fixed(_) | Self::Star | Self::Plus)
    }
}
impl Arg {
    fn optional(&self) -> bool {
        self.names[0].starts_with('-')
    }
    fn from(v: &Value) -> Result<Self, Crash> {
        let names = strings(&field(v, "names")?)?;
        if names.is_empty() {
            return Err(Crash::new("add_argument needs at least one name"));
        }
        let optional = names[0].starts_with('-');
        if names.iter().any(|n| {
            n.is_empty()
                || n == "-"
                || n == "--"
                || n.contains('=')
                || n.chars().any(char::is_whitespace)
                || n.starts_with('-') != optional
        }) || (!optional && names.len() != 1)
        {
            return Err(Crash::new(
                "argument names must be one positional name or option aliases",
            ));
        }
        let action = name(&field(v, "action")?)?;
        if ![
            "store",
            "store_true",
            "store_false",
            "store_const",
            "append",
            "append_const",
            "count",
            "help",
            "version",
        ]
        .contains(&action.as_str())
        {
            return Err(Crash::new("unsupported argparse action"));
        }
        if !optional && !["store", "append"].contains(&action.as_str()) {
            return Err(Crash::new("this action requires an option flag"));
        }
        let zero = [
            "store_true",
            "store_false",
            "store_const",
            "append_const",
            "count",
            "help",
            "version",
        ]
        .contains(&action.as_str());
        let arity = field(v, "nargs")?;
        let nargs = if null(&arity) {
            if zero {
                Nargs::Zero
            } else {
                Nargs::One
            }
        } else if zero {
            return Err(Crash::new("flag actions do not accept nargs"));
        } else {
            match arity {
                Value::Num(n) if n > 0.0 && n.fract() == 0.0 && n <= 1_000_000.0 => {
                    Nargs::Fixed(n as usize)
                }
                v => match name(&v)?.as_str() {
                    "?" => Nargs::Optional,
                    "*" => Nargs::Star,
                    "+" => Nargs::Plus,
                    _ => return Err(Crash::new("nargs must be a positive integer, ?, *, or +")),
                },
            }
        };
        let kind = name(&field(v, "type")?)?;
        if !["string", "int", "float"].contains(&kind.as_str()) {
            return Err(Crash::new("argument type must be :string, :int, or :float"));
        }
        let dest = field(v, "dest")?;
        let dest = if null(&dest) {
            names
                .iter()
                .find(|n| n.starts_with("--"))
                .unwrap_or(&names[0])
                .trim_start_matches('-')
                .replace('-', "_")
        } else {
            name(&dest)?
        };
        if dest.is_empty() {
            return Err(Crash::new("argument destination cannot be empty"));
        }
        let choices = field(v, "choices")?;
        let choices = if null(&choices) {
            None
        } else {
            Some(list(&choices)?)
        };
        let metavar = field(v, "metavar")?;
        let metavar = if null(&metavar) {
            if optional {
                dest.to_uppercase()
            } else {
                dest.clone()
            }
        } else {
            text(&metavar)?
        };
        Ok(Self {
            names,
            dest,
            help: text(&field(v, "help")?)?,
            kind,
            action,
            default: field(v, "default")?,
            required: field(v, "required")?.truthy(),
            nargs,
            choices,
            metavar,
            constant: field(v, "const")?,
            version: text(&field(v, "version")?)?,
        })
    }
    fn convert(&self, s: &str) -> Result<Value, String> {
        let value = match self.kind.as_str() {
            "int" => {
                let n = s
                    .parse::<i64>()
                    .map_err(|_| format!("{}: invalid int value: {s}", self.names[0]))?;
                if n.unsigned_abs() > 9_007_199_254_740_991 {
                    return Err(format!(
                        "{}: integer is outside the exact numeric range",
                        self.names[0]
                    ));
                }
                Value::Num(n as f64)
            }
            "float" => {
                let n = s
                    .parse::<f64>()
                    .map_err(|_| format!("{}: invalid float value: {s}", self.names[0]))?;
                if !n.is_finite() {
                    return Err(format!("{}: expected a finite float", self.names[0]));
                }
                Value::Num(n)
            }
            _ => string(s),
        };
        if self
            .choices
            .as_ref()
            .is_some_and(|choices| !choices.iter().any(|c| deep_equal(c, &value)))
        {
            return Err(format!("{}: invalid choice: {s}", self.names[0]));
        }
        Ok(value)
    }
    fn initial(&self) -> Result<Value, String> {
        if !null(&self.default) {
            return match &self.default {
                Value::Str(s) => self.convert(s),
                v => Ok(copy_value(v)),
            };
        }
        Ok(match self.action.as_str() {
            "store_true" => boolean(false),
            "store_false" => boolean(true),
            "count" => Value::Num(0.0),
            "append" | "append_const" => new_list(Vec::new()),
            _ if matches!(self.nargs, Nargs::Star) => new_list(Vec::new()),
            _ => Value::null(),
        })
    }
    fn render_value(&self) -> String {
        match self.nargs {
            Nargs::Zero => String::new(),
            Nargs::One => self.metavar.clone(),
            Nargs::Fixed(n) => vec![self.metavar.clone(); n.min(8)].join(" "),
            Nargs::Optional => format!("[{}]", self.metavar),
            Nargs::Star => format!("[{} ...]", self.metavar),
            Nargs::Plus => format!("{} [{} ...]", self.metavar, self.metavar),
        }
    }
}

fn specs(p: &Value) -> Result<Vec<Arg>, Crash> {
    parser(p)?;
    list(&field(p, "arguments")?)?
        .iter()
        .map(Arg::from)
        .collect()
}
fn validate(p: &Value) -> Result<Vec<Arg>, Crash> {
    let specs = specs(p)?;
    let mut names = std::collections::HashSet::new();
    let mut dests = std::collections::HashSet::new();
    if field(p, "add_help")?.truthy() {
        names.insert("-h".to_owned());
        names.insert("--help".to_owned());
    }
    for a in &specs {
        for n in &a.names {
            if !names.insert(n.clone()) {
                return Err(Crash::new(format!("duplicate argument name: {n}")));
            }
        }
        if !dests.insert(a.dest.clone()) {
            return Err(Crash::new(format!(
                "duplicate argument destination: {}",
                a.dest
            )));
        }
        a.initial().map_err(Crash::new)?;
    }
    Ok(specs)
}
fn usage(p: &Value, args: &[Arg]) -> Result<String, Crash> {
    let mut parts = vec![format!("usage: {}", text(&field(p, "prog")?)?)];
    if field(p, "add_help")?.truthy() {
        parts.push("[-h]".into());
    }
    for a in args {
        let mut part = if a.optional() {
            a.names.last().unwrap().clone()
        } else {
            String::new()
        };
        let values = a.render_value();
        if !values.is_empty() {
            if !part.is_empty() {
                part.push(' ');
            }
            part.push_str(&values);
        }
        if a.optional() && !a.required {
            part = format!("[{part}]");
        }
        parts.push(part);
    }
    let commands = list(&field(p, "commands")?)?;
    if !commands.is_empty() {
        let names = commands
            .iter()
            .map(|c| text(&field(c, "name")?))
            .collect::<Result<Vec<_>, Crash>>()?;
        parts.push(format!("{{{}}} ...", names.join(",")));
    }
    Ok(parts.join(" ") + "\n")
}
fn help(p: &Value, args: &[Arg]) -> Result<String, Crash> {
    let mut out = usage(p, args)?;
    let description = text(&field(p, "description")?)?;
    if !description.is_empty() {
        out += &format!("\n{description}\n");
    }
    for (title, optional) in [("positional arguments", false), ("options", true)] {
        let mut rows = Vec::new();
        if optional && field(p, "add_help")?.truthy() {
            rows.push((
                "-h, --help".into(),
                "show this help message and exit".into(),
            ));
        }
        for a in args.iter().filter(|a| a.optional() == optional) {
            let label = if optional {
                let suffix = a.render_value();
                a.names
                    .iter()
                    .map(|name| {
                        if suffix.is_empty() {
                            name.clone()
                        } else {
                            format!("{name} {suffix}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                a.metavar.clone()
            };
            let mut desc = a.help.clone();
            if let Some(choices) = &a.choices {
                desc += &format!(
                    " (choices: {})",
                    choices
                        .iter()
                        .map(crate::value::to_text)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            rows.push((label, desc));
        }
        if !rows.is_empty() {
            out += &format!("\n{title}:\n");
            for (label, desc) in rows {
                out += &format!("  {label:24} {desc}\n");
            }
        }
    }
    let commands = list(&field(p, "commands")?)?;
    if !commands.is_empty() {
        out += "\ncommands:\n";
        for c in commands {
            out += &format!(
                "  {:24} {}\n",
                text(&field(&c, "name")?)?,
                text(&field(&c, "help")?)?
            );
        }
    }
    let epilog = text(&field(p, "epilog")?)?;
    if !epilog.is_empty() {
        out += &format!("\n{epilog}\n");
    }
    Ok(out)
}

struct Parsed {
    values: Value,
    unknown: Vec<String>,
}
enum ParseError {
    Input(String),
    Display(String),
    Formatted(String),
}
impl From<String> for ParseError {
    fn from(s: String) -> Self {
        Self::Input(s)
    }
}
fn is_option(s: &str, args: &[Arg]) -> bool {
    if args.iter().any(|a| a.names.iter().any(|n| n == s)) {
        return true;
    }
    s.starts_with('-') && s != "-" && s.parse::<f64>().is_err()
}
fn apply(a: &Arg, raw: &[String], values: &mut Value) -> Result<(), String> {
    let result = match a.action.as_str() {
        "store_true" => boolean(true),
        "store_false" => boolean(false),
        "store_const" | "append_const" => copy_value(&a.constant),
        "count" => {
            let current = field(values, &a.dest).map_err(|e| e.message)?;
            Value::Num(number(&current).map_err(|e| e.message)? + 1.0)
        }
        _ => {
            let converted = raw
                .iter()
                .map(|s| a.convert(s))
                .collect::<Result<Vec<_>, _>>()?;
            if a.nargs.multiple() {
                new_list(converted)
            } else {
                match converted.into_iter().next() {
                    Some(value) => value,
                    None => match &a.constant {
                        Value::Str(value) => a.convert(value)?,
                        value => copy_value(value),
                    },
                }
            }
        }
    };
    if matches!(a.action.as_str(), "append" | "append_const") {
        let current = field(values, &a.dest).map_err(|e| e.message)?;
        let mut values_list =
            list(&current).map_err(|_| format!("{}: append default must be a list", a.names[0]))?;
        values_list.push(result);
        put(values, &a.dest, new_list(values_list)).map_err(|e| e.message)
    } else {
        put(values, &a.dest, result).map_err(|e| e.message)
    }
}

fn parse(
    p: &Value,
    args: &[Arg],
    tokens: &[String],
    known: bool,
    depth: usize,
) -> Result<Parsed, ParseError> {
    if depth > 32 {
        return Err("subcommands nested too deeply".to_owned().into());
    }
    let crash = |e: Crash| ParseError::Input(e.message);
    let mut values = new_dict(Vec::new());
    for a in args {
        put(&mut values, &a.dest, a.initial()?).map_err(crash)?;
    }
    let commands = list(&field(p, "commands").map_err(crash)?).map_err(crash)?;
    if !commands.is_empty() {
        let dest = text(&field(p, "command_dest").map_err(crash)?).map_err(crash)?;
        put(&mut values, &dest, Value::null()).map_err(crash)?;
    }
    let positional_min: usize = args
        .iter()
        .filter(|a| !a.optional())
        .map(|a| a.nargs.min())
        .sum();
    let mut unknown: Vec<(usize, String)> = Vec::new();
    let mut positional: Vec<(usize, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stopped = false;
    let mut child_values = None;
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if !stopped && token == "--" {
            stopped = true;
            i += 1;
            continue;
        }
        if !stopped
            && field(p, "add_help").map_err(crash)?.truthy()
            && ["-h", "--help"].contains(&token.as_str())
        {
            return Err(ParseError::Display(help(p, args).map_err(crash)?));
        }
        if !is_option(token, args) && positional.len() >= positional_min {
            if let Some(command) = commands.iter().find(|c| {
                field(c, "name").and_then(|v| text(&v)).ok().as_deref() == Some(token.as_str())
            }) {
                let mut child = field(command, "parser").map_err(crash)?;
                let prog = format!(
                    "{} {token}",
                    text(&field(p, "prog").map_err(crash)?).map_err(crash)?
                );
                put(&mut child, "prog", string(prog)).map_err(crash)?;
                let child_args = validate(&child).map_err(crash)?;
                let parsed = parse(&child, &child_args, &tokens[i + 1..], known, depth + 1)
                    .map_err(|error| match error {
                        ParseError::Input(message) => ParseError::Formatted(format!(
                            "{}{}: error: {message}\n",
                            usage(&child, &child_args).unwrap_or_default(),
                            text(&field(&child, "prog").unwrap()).unwrap()
                        )),
                        other => other,
                    })?;
                put(
                    &mut values,
                    &text(&field(p, "command_dest").map_err(crash)?).map_err(crash)?,
                    string(token),
                )
                .map_err(crash)?;
                unknown.extend(
                    parsed
                        .unknown
                        .into_iter()
                        .enumerate()
                        .map(|(j, s)| (i + 1 + j, s)),
                );
                child_values = Some(parsed.values);
                break;
            }
        }
        if stopped || !is_option(token, args) {
            positional.push((i, token.clone()));
            i += 1;
            continue;
        }
        let (flag, attached) = token
            .split_once('=')
            .map(|(a, b)| (a, Some(b.to_string())))
            .unwrap_or((token.as_str(), None));
        let exact = args
            .iter()
            .position(|a| a.optional() && a.names.iter().any(|n| n == flag));
        let mut pending = Vec::new();
        if let Some(index) = exact {
            pending.push((index, attached));
        } else if token.starts_with('-') && !token.starts_with("--") && token.len() > 2 {
            let mut chars = token[1..].char_indices().peekable();
            let mut valid = true;
            while let Some((_, c)) = chars.next() {
                if c == 'h' && field(p, "add_help").map_err(crash)?.truthy() {
                    return Err(ParseError::Display(help(p, args).map_err(crash)?));
                }
                let short = format!("-{c}");
                let Some(index) = args
                    .iter()
                    .position(|a| a.optional() && a.names.contains(&short))
                else {
                    valid = false;
                    break;
                };
                let a = &args[index];
                let tail = if !matches!(a.nargs, Nargs::Zero) {
                    chars.peek().map(|(offset, _)| {
                        let tail = &token[1 + *offset..];
                        tail.strip_prefix('=').unwrap_or(tail).to_string()
                    })
                } else {
                    None
                };
                pending.push((index, tail));
                if !matches!(a.nargs, Nargs::Zero) {
                    break;
                }
            }
            if !valid {
                pending.clear();
            }
        }
        if pending.is_empty() {
            if known {
                unknown.push((i, token.clone()));
                i += 1;
                continue;
            }
            return Err(format!("unrecognized argument: {token}").into());
        }
        i += 1;
        for (index, attached) in pending {
            let a = &args[index];
            seen.insert(index);
            if a.action == "help" {
                return Err(ParseError::Display(help(p, args).map_err(crash)?));
            }
            if a.action == "version" {
                return Err(ParseError::Display(a.version.clone() + "\n"));
            }
            let mut raw = Vec::new();
            if let Some(v) = attached {
                if matches!(a.nargs, Nargs::Zero) {
                    return Err(format!("{} does not take a value", a.names[0]).into());
                }
                raw.push(v);
            }
            let max = match a.nargs {
                Nargs::Zero => 0,
                Nargs::One | Nargs::Optional => 1,
                Nargs::Fixed(n) => n,
                Nargs::Star | Nargs::Plus => usize::MAX,
            };
            while raw.len() < max && i < tokens.len() && !is_option(&tokens[i], args) {
                raw.push(tokens[i].clone());
                i += 1;
            }
            if raw.len() < a.nargs.min() {
                return Err(format!("{}: expected {} value(s)", a.names[0], a.nargs.min()).into());
            }
            apply(a, &raw, &mut values)?;
        }
    }
    let positionals: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| !a.optional())
        .collect();
    let mut at = 0;
    for (j, (index, a)) in positionals.iter().enumerate() {
        let reserve: usize = positionals[j + 1..]
            .iter()
            .map(|(_, a)| a.nargs.min())
            .sum();
        let remaining = positional.len().saturating_sub(at);
        let available = remaining.saturating_sub(reserve);
        let n = match a.nargs {
            Nargs::One => usize::from(remaining > 0),
            Nargs::Fixed(n) => n.min(remaining),
            Nargs::Optional => available.min(1),
            Nargs::Star | Nargs::Plus => available,
            Nargs::Zero => 0,
        };
        if n < a.nargs.min() {
            return Err(format!("missing required argument: {}", a.names[0]).into());
        }
        if n > 0 || (matches!(a.nargs, Nargs::Star) && null(&a.default)) {
            let raw = positional[at..at + n]
                .iter()
                .map(|(_, s)| s.clone())
                .collect::<Vec<_>>();
            apply(a, &raw, &mut values)?;
            seen.insert(*index);
        }
        at += n;
    }
    unknown.extend(positional[at..].iter().cloned());
    unknown.sort_by_key(|(i, _)| *i);
    if !known && !unknown.is_empty() {
        return Err(format!(
            "unrecognized arguments: {}",
            unknown
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )
        .into());
    }
    for (index, a) in args.iter().enumerate() {
        if a.required && !seen.contains(&index) {
            return Err(format!("missing required argument: {}", a.names[0]).into());
        }
    }
    if !commands.is_empty()
        && child_values.is_none()
        && field(p, "command_required").map_err(crash)?.truthy()
    {
        return Err("a command is required".to_owned().into());
    }
    if let Some(child) = child_values {
        let Value::Dict(child) = child else {
            unreachable!()
        };
        for (k, v) in dict_entries(&child) {
            put(&mut values, k.name(), copy_value(&v)).map_err(crash)?;
        }
    }
    Ok(Parsed {
        values,
        unknown: unknown.into_iter().map(|(_, s)| s).collect(),
    })
}

pub(crate) fn call(
    native: Native,
    args: &[Option<Value>],
    script_args: &[String],
    prog: &str,
) -> Result<Outcome, Crash> {
    let arg = |name| argument(args, native, name);
    let default = |name, v| arg(name).unwrap_or(v);
    let one = |v| Ok(Outcome::Values(vec![v]));
    match native {
        Native::CliParser => {
            let given = arg("prog").unwrap_or_else(Value::null);
            let prog = if null(&given) {
                string(prog)
            } else {
                string(text(&given)?)
            };
            one(new_dict(vec![
                (sym("_kind"), Value::Sym(sym("cli_parser"))),
                (sym("prog"), prog),
                (sym("description"), default("description", string(""))),
                (sym("epilog"), default("epilog", string(""))),
                (sym("add_help"), default("add_help", boolean(true))),
                (sym("arguments"), new_list(Vec::new())),
                (sym("commands"), new_list(Vec::new())),
                (sym("command_dest"), string("command")),
                (sym("command_required"), boolean(true)),
            ]))
        }
        Native::CliAddArgument | Native::CliAddSubparser => {
            let Value::Ref(target) = arg("parser").expect("parser ref") else {
                unreachable!()
            };
            let mut p = read_place(&target.root, &target.path)?;
            parser(&p)?;
            if native == Native::CliAddArgument {
                let fields = [
                    ("names", arg("names").expect("variadic")),
                    ("help", default("help", string(""))),
                    ("dest", default("dest", Value::null())),
                    ("type", default("type", Value::Sym(sym("string")))),
                    ("default", default("default", Value::null())),
                    ("required", default("required", boolean(false))),
                    ("action", default("action", Value::Sym(sym("store")))),
                    ("nargs", default("nargs", Value::null())),
                    ("choices", default("choices", Value::null())),
                    ("metavar", default("metavar", Value::null())),
                    ("const", default("const", Value::null())),
                    ("version", default("version", string(""))),
                ];
                let spec = new_dict(fields.into_iter().map(|(k, v)| (sym(k), v)).collect());
                let mut arguments = list(&field(&p, "arguments")?)?;
                arguments.push(spec);
                put(&mut p, "arguments", new_list(arguments))?;
                validate(&p)?;
            } else {
                let name = text(&arg("name").expect("name"))?;
                if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_whitespace)
                {
                    return Err(Crash::new("invalid subcommand name"));
                }
                let child = arg("child").expect("child");
                validate(&child)?;
                let mut commands = list(&field(&p, "commands")?)?;
                if commands
                    .iter()
                    .any(|c| field(c, "name").and_then(|v| text(&v)).ok().as_deref() == Some(&name))
                {
                    return Err(Crash::new("duplicate subcommand"));
                }
                commands.push(new_dict(vec![
                    (sym("name"), string(name)),
                    (sym("parser"), copy_value(&child)),
                    (sym("help"), default("help", string(""))),
                ]));
                put(&mut p, "commands", new_list(commands))?;
                put(&mut p, "command_dest", default("dest", string("command")))?;
                put(
                    &mut p,
                    "command_required",
                    default("required", boolean(true)),
                )?;
            }
            write_place(&target.root, &target.path, p)?;
            one(Value::null())
        }
        _ => {
            let p = arg("parser").expect("parser");
            let specs = validate(&p)?;
            if native == Native::CliFormatHelp {
                return one(string(help(&p, &specs)?));
            }
            if native == Native::CliFormatUsage {
                return one(string(usage(&p, &specs)?));
            }
            let tokens = match arg("args") {
                None => script_args.to_vec(),
                Some(v) if null(&v) => script_args.to_vec(),
                Some(v) => strings(&v)?,
            };
            let known = !default("strict", boolean(true)).truthy();
            match parse(&p, &specs, &tokens, known, 0) {
                Ok(parsed) => Ok(Outcome::Values(vec![
                    parsed.values,
                    new_list(parsed.unknown.into_iter().map(string).collect()),
                ])),
                Err(error) => {
                    let (code, message) = match error {
                        ParseError::Display(message) => (0, message),
                        ParseError::Formatted(message) => (2, message),
                        ParseError::Input(error) => (
                            2,
                            format!(
                                "{}{}: error: {error}\n",
                                usage(&p, &specs)?,
                                text(&field(&p, "prog")?)?
                            ),
                        ),
                    };
                    Ok(Outcome::Exit { code, message })
                }
            }
        }
    }
}
