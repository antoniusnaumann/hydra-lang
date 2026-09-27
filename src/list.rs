//! Eager list operations; callback implementations share the normal VM stack.
use crate::compile::{compile_program, Chunk, Instr};
use crate::errors::Crash;
use crate::scope::Scope;
use crate::stdlib::{argument, number, string, text};
use crate::value::{
    copy_value, deep_equal, deref, list_items, new_list, Closure, Native, Value, NATIVES,
};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const MAX_GENERATED: usize = 1_000_000;
fn items(v: &Value) -> Result<Vec<Value>, Crash> {
    match deref(v)? {
        Value::List(v) => Ok(list_items(&v).iter().map(copy_value).collect()),
        _ => Err(Crash::new("list operation requires a list")),
    }
}
fn count(v: &Value, positive: bool) -> Result<usize, Crash> {
    let n = number(v)?;
    if n.fract() != 0.0 || n < if positive { 1.0 } else { 0.0 } || n > 9_007_199_254_740_991.0 {
        return Err(Crash::new(if positive {
            "expected a positive integer"
        } else {
            "expected a nonnegative integer"
        }));
    }
    Ok(n as usize)
}
fn capacity(n: usize) -> Result<(), Crash> {
    if n > MAX_GENERATED {
        Err(Crash::new("generated list exceeds 1,000,000 elements"))
    } else {
        Ok(())
    }
}
fn compare(a: &Value, b: &Value, depth: usize) -> Result<Ordering, Crash> {
    if depth > 128 {
        return Err(Crash::new("list comparison nested too deeply"));
    }
    match (deref(a)?, deref(b)?) {
        (Value::Num(a), Value::Num(b)) => a
            .partial_cmp(&b)
            .ok_or_else(|| Crash::new("cannot order NaN")),
        (Value::Str(a), Value::Str(b)) => Ok(a.cmp(&b)),
        (Value::Sym(a), Value::Sym(b)) => Ok(a.name().cmp(b.name())),
        (Value::List(a), Value::List(b)) => {
            let (a, b) = (list_items(&a), list_items(&b));
            for (a, b) in a.iter().zip(&b) {
                let cmp = compare(a, b, depth + 1)?;
                if cmp != Ordering::Equal {
                    return Ok(cmp);
                }
            }
            Ok(a.len().cmp(&b.len()))
        }
        _ => Err(Crash::new(
            "ordering requires matching numbers, strings, atoms, or lists",
        )),
    }
}

fn sorted(mut values: Vec<Value>) -> Result<Vec<Value>, Crash> {
    if values.len() < 2 {
        return Ok(values);
    }
    let right = values.split_off(values.len() / 2);
    let mut left = sorted(values)?.into_iter().peekable();
    let mut right = sorted(right)?.into_iter().peekable();
    let mut result = Vec::new();
    while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
        result.push(if compare(a, b, 0)? != Ordering::Greater {
            left.next().unwrap()
        } else {
            right.next().unwrap()
        });
    }
    result.extend(left);
    result.extend(right);
    Ok(result)
}

/// Compile once, then enter a fresh closure in the caller's task, never a nested VM.
pub(crate) fn script(native: Native, args: &[Option<Value>]) -> Result<Option<Value>, Crash> {
    static CHUNKS: OnceLock<Result<HashMap<String, Arc<Chunk>>, Crash>> = OnceLock::new();
    let chunks = CHUNKS.get_or_init(|| {
        let program = crate::parser::parse(include_str!("list.hy"), "<stdlib/list.hy>")
            .map_err(|e| Crash::new(e.to_string()))?;
        let main = compile_program(&program).map_err(|e| Crash::new(e.to_string()))?;
        Ok(main
            .code
            .iter()
            .filter_map(|i| match i {
                Instr::MakeClosure { name, chunk, .. } => Some((name.to_string(), chunk.clone())),
                _ => None,
            })
            .collect())
    });
    let chunks = chunks.as_ref().map_err(Clone::clone)?;
    let Some(chunk) = chunks.get(native.name()) else {
        return Ok(None);
    };
    let _ = items(&argument(args, native, "items").expect("items"))?;
    let callback = argument(args, native, "f")
        .or_else(|| argument(args, native, "compare"))
        .expect("callback");
    if !matches!(deref(&callback)?, Value::Fn(_) | Value::Native(_)) {
        return Err(Crash::new("list callback must be a function"));
    }
    let scope = Scope::root();
    for native in NATIVES.iter().filter(|n| n.module() == Some("list")) {
        scope.declare(native.name(), Value::Native(*native));
    }
    Ok(Some(Value::Fn(Arc::new(Closure {
        name: format!("list::{}", native.name()),
        params: chunk.params.clone(),
        chunk: chunk.clone(),
        scope,
    }))))
}

pub(crate) fn call(native: Native, args: &[Option<Value>]) -> Result<Vec<Value>, Crash> {
    let arg = |name| argument(args, native, name).unwrap_or_else(Value::null);
    let list = |name| items(&arg(name));
    let one = |v| Ok(vec![v]);
    let out = match native {
        Native::ListRange | Native::ListRangeTo => {
            let start = if native == Native::ListRangeTo {
                0.0
            } else {
                number(&arg("start"))?
            };
            let end = number(&arg("end"))?;
            let step = argument(args, native, "step")
                .map(|v| number(&v))
                .transpose()?
                .unwrap_or(1.0);
            if [start, end, step]
                .iter()
                .any(|n| n.fract() != 0.0 || n.abs() > 9_007_199_254_740_991.0)
                || step == 0.0
            {
                return Err(Crash::new(
                    "range requires safe integers and a nonzero step",
                ));
            }
            let length = ((end - start) / step).ceil().max(0.0);
            if length > MAX_GENERATED as f64 {
                return Err(Crash::new("generated list exceeds 1,000,000 elements"));
            }
            (0..length as usize)
                .map(|i| Value::Num(start + i as f64 * step))
                .collect()
        }
        Native::ListRepeat => {
            let n = count(&arg("count"), false)?;
            capacity(n)?;
            (0..n).map(|_| copy_value(&arg("value"))).collect()
        }
        Native::ListChain
        | Native::ListZip
        | Native::ListInterleave
        | Native::ListCartesianProduct => {
            let (a, b) = (list("left")?, list("right")?);
            match native {
                Native::ListChain => {
                    capacity(a.len().saturating_add(b.len()))?;
                    a.into_iter().chain(b).collect()
                }
                Native::ListZip => a
                    .into_iter()
                    .zip(b)
                    .map(|(a, b)| new_list(vec![a, b]))
                    .collect(),
                Native::ListInterleave => {
                    capacity(a.len().saturating_add(b.len()))?;
                    let mut out = Vec::new();
                    for i in 0..a.len().max(b.len()) {
                        if let Some(v) = a.get(i) {
                            out.push(copy_value(v));
                        }
                        if let Some(v) = b.get(i) {
                            out.push(copy_value(v));
                        }
                    }
                    out
                }
                _ => {
                    capacity(a.len().saturating_mul(b.len()))?;
                    a.iter()
                        .flat_map(|a| {
                            b.iter()
                                .map(move |b| new_list(vec![copy_value(a), copy_value(b)]))
                        })
                        .collect()
                }
            }
        }
        _ => {
            let mut values = list("items")?;
            match native {
                Native::ListFlatten => {
                    let mut out = Vec::new();
                    for value in values {
                        let child = items(&value)?;
                        capacity(out.len().saturating_add(child.len()))?;
                        out.extend(child);
                    }
                    out
                }
                Native::ListEnumerate => values
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| new_list(vec![Value::Num(i as f64), v]))
                    .collect(),
                Native::ListRev => {
                    values.reverse();
                    values
                }
                Native::ListCount => return one(Value::Num(values.len() as f64)),
                Native::ListFirst | Native::ListLast | Native::ListNth => {
                    return one(match native {
                        Native::ListFirst => values.first(),
                        Native::ListLast => values.last(),
                        _ => values.get(count(&arg("index"), false)?),
                    }
                    .map(copy_value)
                    .unwrap_or_else(Value::null))
                }
                Native::ListTake => values
                    .into_iter()
                    .take(count(&arg("count"), false)?)
                    .collect(),
                Native::ListSkip => values
                    .into_iter()
                    .skip(count(&arg("count"), false)?)
                    .collect(),
                Native::ListStepBy => values
                    .into_iter()
                    .step_by(count(&arg("step"), true)?)
                    .collect(),
                Native::ListChunks => values
                    .chunks(count(&arg("size"), true)?)
                    .map(|v| new_list(v.iter().map(copy_value).collect()))
                    .collect(),
                Native::ListWindows => {
                    let size = count(&arg("size"), true)?;
                    let windows = values.len().checked_sub(size).map_or(0, |n| n + 1);
                    capacity(windows.saturating_mul(size))?;
                    values
                        .windows(size)
                        .map(|v| new_list(v.iter().map(copy_value).collect()))
                        .collect()
                }
                Native::ListIntersperse => {
                    capacity(values.len().saturating_mul(2).saturating_sub(1))?;
                    let mut out = Vec::new();
                    for v in values {
                        if !out.is_empty() {
                            out.push(copy_value(&arg("separator")));
                        }
                        out.push(v);
                    }
                    out
                }
                Native::ListUnzip => {
                    let (mut a, mut b) = (Vec::new(), Vec::new());
                    for v in values {
                        let pair = items(&v)?;
                        if pair.len() != 2 {
                            return Err(Crash::new("unzip expects two-element lists"));
                        }
                        a.push(copy_value(&pair[0]));
                        b.push(copy_value(&pair[1]));
                    }
                    return Ok(vec![new_list(a), new_list(b)]);
                }
                Native::ListSum | Native::ListProduct => {
                    let mut total = if native == Native::ListSum { 0.0 } else { 1.0 };
                    for v in values {
                        let n = number(&v)?;
                        if native == Native::ListSum {
                            total += n
                        } else {
                            total *= n
                        }
                    }
                    return one(Value::Num(total));
                }
                Native::ListMin | Native::ListMax => {
                    let mut result: Option<Value> = None;
                    for v in values {
                        let cmp = if let Some(best) = &result {
                            compare(&v, best, 0)?
                        } else {
                            compare(&v, &v, 0)?;
                            Ordering::Equal
                        };
                        if result.is_none()
                            || (native == Native::ListMin && cmp == Ordering::Less)
                            || (native == Native::ListMax && cmp != Ordering::Less)
                        {
                            result = Some(v);
                        }
                    }
                    return one(result.unwrap_or_else(Value::null));
                }
                Native::ListSorted => {
                    for v in &values {
                        compare(v, v, 0)?;
                    }
                    sorted(values)?
                }
                Native::ListUnique | Native::ListDedup | Native::ListCounts => {
                    let mut out: Vec<Value> = Vec::new();
                    let mut counts: Vec<usize> = Vec::new();
                    for v in values {
                        let found = if native == Native::ListDedup {
                            out.last()
                                .filter(|p| deep_equal(p, &v))
                                .map(|_| out.len() - 1)
                        } else {
                            out.iter().position(|p| deep_equal(p, &v))
                        };
                        if let Some(index) = found {
                            counts[index] += 1;
                        } else {
                            out.push(v);
                            counts.push(1);
                        }
                    }
                    if native == Native::ListCounts {
                        out.into_iter()
                            .zip(counts)
                            .map(|(v, n)| new_list(vec![v, Value::Num(n as f64)]))
                            .collect()
                    } else {
                        out
                    }
                }
                Native::ListJoin => {
                    let sep = argument(args, native, "separator")
                        .map(|v| text(&v))
                        .transpose()?
                        .unwrap_or_default();
                    return one(string(
                        values
                            .iter()
                            .map(crate::value::to_text)
                            .collect::<Vec<_>>()
                            .join(&sep),
                    ));
                }
                Native::ListCombinations | Native::ListPermutations => {
                    let k = count(&arg("size"), false)?;
                    if k > values.len() {
                        return one(new_list(Vec::new()));
                    }
                    let mut indices: Vec<usize> = (0..k).collect();
                    let mut out = Vec::new();
                    if native == Native::ListCombinations {
                        loop {
                            capacity(out.len().saturating_add(1).saturating_mul(k.max(1)))?;
                            out.push(new_list(
                                indices.iter().map(|i| copy_value(&values[*i])).collect(),
                            ));
                            let Some(i) =
                                (0..k).rev().find(|i| indices[*i] < values.len() - k + *i)
                            else {
                                break;
                            };
                            indices[i] += 1;
                            for j in i + 1..k {
                                indices[j] = indices[j - 1] + 1;
                            }
                        }
                    } else {
                        // Iterative depth-first index permutations; no recursion on requested size.
                        indices.clear();
                        let mut used = vec![false; values.len()];
                        let mut next = vec![0; k + 1];
                        loop {
                            let depth = indices.len();
                            if depth == k {
                                capacity(out.len().saturating_add(1).saturating_mul(k.max(1)))?;
                                out.push(new_list(
                                    indices.iter().map(|i| copy_value(&values[*i])).collect(),
                                ));
                            }
                            if depth < k {
                                let mut i = next[depth];
                                while i < values.len() && used[i] {
                                    i += 1;
                                }
                                if i < values.len() {
                                    next[depth] = i + 1;
                                    used[i] = true;
                                    indices.push(i);
                                    next[depth + 1] = 0;
                                    continue;
                                }
                            }
                            let Some(i) = indices.pop() else { break };
                            used[i] = false;
                        }
                    }
                    out
                }
                _ => unreachable!("callback native enters a VM frame"),
            }
        }
    };
    one(new_list(out))
}
