//! HTTP(S) requests with bounded bodies and fs-style fallback overloads.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use ureq::ResponseExt;

use crate::errors::Crash;
use crate::stdlib::{argument, duration, failure, number, string, success, text};
use crate::value::{deref, dict_entries, list_items, new_dict, new_list, sym, Native, Value};

fn reason(error: &ureq::Error) -> &'static str {
    match error {
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::Io(e) if e.get_ref().is_some_and(|e| e.is::<rustls::Error>()) => "tls",
        ureq::Error::HostNotFound => "dns",
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "tls",
        ureq::Error::BodyExceedsLimit(_) => "too_large",
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => "redirect",
        _ => "network",
    }
}

pub(crate) fn call(native: Native, args: &[Option<Value>]) -> Result<Vec<Value>, Crash> {
    let arg = |name| argument(args, native, name);
    let fallback = arg("fallback");
    let download_path = arg("path").map(|v| text(&v)).transpose()?;
    let url = text(&arg("url").expect("url"))?;
    let uri: ureq::http::Uri = url.parse().map_err(|_| Crash::new("invalid HTTP URL"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
        return Err(Crash::new(
            "URL must have an http or https scheme and a host",
        ));
    }
    let method = arg("method")
        .map(|v| match deref(&v)? {
            Value::Sym(s) => Ok(s.name().to_ascii_uppercase()),
            v => text(&v).map(|s| s.to_ascii_uppercase()),
        })
        .transpose()?
        .unwrap_or_else(|| {
            if native.name() == "post" {
                "POST"
            } else {
                "GET"
            }
            .into()
        });
    let body = arg("body")
        .map(|v| match deref(&v)? {
            Value::Sym(s) if s.name() == "null" => Ok(String::new()),
            v => text(&v),
        })
        .transpose()?
        .unwrap_or_default();
    let timeout = duration(&arg("timeout").unwrap_or(Value::Num(30.0)))?;
    if timeout.is_zero() {
        return Err(Crash::new("HTTP timeout must be positive"));
    }
    let max_bytes = number(&arg("max_bytes").unwrap_or(Value::Num(16_777_216.0)))?;
    if max_bytes < 0.0 || max_bytes.fract() != 0.0 || max_bytes > 9_007_199_254_740_991.0 {
        return Err(Crash::new("max_bytes must be a nonnegative safe integer"));
    }
    let mut request = ureq::http::Request::builder()
        .method(method.as_str())
        .uri(uri);
    if let Some(headers) = arg("headers") {
        let Value::Dict(headers) = deref(&headers)? else {
            return Err(Crash::new("headers must be a dict"));
        };
        for (key, value) in dict_entries(&headers) {
            let values = match deref(&value)? {
                Value::List(v) => list_items(&v),
                v => vec![v],
            };
            for value in values {
                request = request.header(key.name(), text(&value)?);
            }
        }
    }
    let request = request
        .body(body)
        .map_err(|e| Crash::new(format!("invalid HTTP request: {e}")))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into();
    let mut response = match agent.run(request) {
        Ok(r) => r,
        Err(e) => {
            return failure(
                fallback,
                reason(&e),
                format!("http::{}: {e}", native.name()),
            )
        }
    };
    let status = response.status().as_u16();
    if native.name() == "download" && !(200..300).contains(&status) {
        return failure(
            fallback,
            "http_status",
            format!("download returned HTTP {status}"),
        );
    }
    let final_url = response.get_uri().to_string();
    // Header names are lowercase; lists preserve repeated values (not comma-joined).
    let headers = new_dict(
        response
            .headers()
            .keys()
            .map(|key| {
                let values = response
                    .headers()
                    .get_all(key)
                    .iter()
                    .map(|v| string(String::from_utf8_lossy(v.as_bytes()).into_owned()))
                    .collect();
                (sym(key.as_str()), new_list(values))
            })
            .collect(),
    );
    let bytes = match response
        .body_mut()
        .with_config()
        .limit(max_bytes as u64)
        .read_to_vec()
    {
        Ok(bytes) => bytes,
        Err(e) => return failure(fallback, reason(&e), format!("HTTP body: {e}")),
    };
    if native.name() == "download" {
        let path = download_path.expect("download path");
        return match save(&path, &bytes) {
            Ok(()) => success(string(path)),
            Err(e) => failure(
                fallback,
                match e.kind() {
                    std::io::ErrorKind::PermissionDenied => "denied",
                    std::io::ErrorKind::NotFound => "not_found",
                    _ => "io",
                },
                format!("download: {e}"),
            ),
        };
    }
    let body = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => return failure(fallback, "encoding", format!("HTTP body is not UTF-8: {e}")),
    };
    success(new_dict(vec![
        (sym("status"), Value::Num(status as f64)),
        (sym("headers"), headers),
        (sym("body"), string(body)),
        (sym("url"), string(final_url)),
    ]))
}

/// Publish only complete downloads; a failed request never truncates the target.
fn save(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let target = Path::new(path);
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let (temp, mut file) = loop {
        let path = parent.join(format!(
            ".hydra-download-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => break (Temp(path), file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp.0, target)
}
